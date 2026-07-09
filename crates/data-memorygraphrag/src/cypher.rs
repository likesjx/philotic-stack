// Pure Cypher compilation for Life Graph OS observe operations.
// No I/O — all functions are deterministic and fully testable.

use crate::{
    ConflictHandoff, ConflictHandoffStatus, FeedbackEdgeSpec, LifeCommitInput, LifeObserveInput,
    LifePatchProposalInput, LifeResolveInput, PatchKind, RetrievalFeedbackInput,
    RetrievalFeedbackRating, SourceKind, ValidationState,
};

const KNOWN_LABELS: &[&str] = &[
    "Person",
    "Role",
    "Aspiration",
    "Goal",
    "System",
    "Habit",
    "Project",
    "Commitment",
    "OpenLoop",
    "NextAction",
    "Routine",
    "Decision",
    "Preference",
    "Value",
    "Concern",
    "Event",
    "Signal",
    "GrowthHypothesis",
    "GrowthExperiment",
    "DriftFinding",
    "CapabilityPatch",
    "SkillPatch",
    "ToolPatch",
    "SchemaPatch",
    "AttentionPatch",
    "SystemPatch",
    "StewardshipInstruction",
];

/// Living-cycle relationship types allowed on `life.observe` edge writes.
/// The soft-zoning design routes all agent domains through this small,
/// closed vocabulary; anything else is rejected before the node write.
pub const LIVING_CYCLE_REL_TYPES: &[&str] = &["OWNS", "SHAPES", "SETS", "SPAWNS", "RELATES_TO"];

pub fn is_living_cycle_rel_type(rel_type: &str) -> bool {
    LIVING_CYCLE_REL_TYPES.contains(&rel_type)
}

/// Fallback written to `observed_by` when the caller predates per-agent provenance.
pub const OBSERVED_BY_UNKNOWN: &str = "agent:unknown";

/// Validated, compiled `life.observe` Cypher ready for execution.
#[derive(Debug)]
pub struct ObserveCypher {
    pub query: String,
    pub node_id: String,
    pub label: String,
    pub confidence: f64,
    pub source_membrane: String,
    pub provenance: String,
    pub observed_by: String,
    pub observed_role: Option<String>,
    pub validation_state: String,
    pub observed_at: String,
    pub created_at: String,
    pub claim_summary: String,
    pub observation_id: String,
    pub packet_id: String,
    /// Muninn origin: engram ID of the first `MuninnEngram` source ref, if any.
    /// Preserved on the node so promotion (lifegraph-muninn-promotion seam)
    /// can trace a Life Graph fact back to its Muninn continuity source.
    pub origin_engram_id: Option<String>,
    /// Muninn origin: reliability score of that source ref (0.0–1.0).
    pub origin_trust: Option<f64>,
}

/// One compiled living-cycle edge MERGE for a `life.observe` request.
/// The MATCH on the target means a missing target creates nothing; the
/// provider reports the miss in the response envelope instead of failing.
#[derive(Debug)]
pub struct ObserveEdgeCypher {
    pub query: String,
    pub rel_type: String,
    pub target_id: String,
}

#[derive(Debug)]
pub struct CommitCypher {
    pub query: String,
    pub node_id: String,
    pub label: String,
    pub packet_id: String,
    pub confidence: f64,
    pub claim_summary: String,
    pub confirmed_at: String,
    /// Empty string sentinel means "leave lifecycle status unchanged" — see
    /// the CASE-empty-string-preserve pattern in `compile_commit`.
    pub loop_status: String,
    pub resolution_note: String,
}

#[derive(Debug)]
pub struct ConflictCypher {
    pub query: String,
    pub handoff_id: String,
    pub conflict_id: String,
    pub status: String,
    pub summary: String,
    pub updated_at: String,
    pub resolution_summary: Option<String>,
    pub handoff_json: String,
}

#[derive(Debug)]
pub struct PatchProposalCypher {
    pub query: String,
    pub patch_id: String,
    pub label: String,
    pub patch_kind: String,
    pub summary: String,
    pub rationale: String,
    pub risk: String,
    pub status: String,
    pub proposed_at: String,
    pub patch_json: String,
}

#[derive(Debug)]
pub struct RecallFeedbackCypher {
    pub query: String,
    pub feedback_id: String,
    pub packet_id: String,
    pub rating: String,
    pub query_summary: String,
    pub note: String,
    pub candidate_count: i64,
    pub connected_candidate_count: i64,
    pub connectivity_ratio: Option<f64>,
    pub feedback_json: String,
    pub evaluation_json: String,
    pub observed_at: String,
}

pub fn compile_observe(input: &LifeObserveInput, now_iso: &str) -> Result<ObserveCypher, String> {
    let label = &input.evidence.claim_ref.label;
    if !is_known_label(label) {
        return Err(format!("unknown Life Graph label: {label}"));
    }

    let source_membrane = input
        .evidence
        .source_refs
        .first()
        .map(|s| s.source_id.clone())
        .or_else(|| {
            input
                .evidence
                .passage_refs
                .first()
                .and_then(|p| p.source_ref_id.clone())
        })
        .unwrap_or_else(|| "agent:memorygraphrag".to_string());

    let provenance = input
        .evidence
        .source_refs
        .first()
        .map(|s| source_kind_to_provenance(&s.source_kind))
        .unwrap_or_else(|| "agent_inferred".to_string());

    // Muninn-origin preservation: the first MuninnEngram source ref pins the
    // origin engram ID and its reliability score onto the node. Non-Muninn
    // writes leave both null; nodes written before this field existed simply
    // never carry it (retrieval treats absence as "no Muninn origin").
    let muninn_origin = input
        .evidence
        .source_refs
        .iter()
        .find(|s| matches!(s.source_kind, SourceKind::MuninnEngram));
    let origin_engram_id = muninn_origin
        .map(|s| s.source_id.clone())
        .filter(|id| !id.trim().is_empty());
    let origin_trust = muninn_origin.map(|s| f64::from(s.reliability.score));

    let validation_state = validation_state_str(&input.evidence.validation_state);

    let observed_at = input
        .evidence
        .observed_at
        .clone()
        .unwrap_or_else(|| now_iso.to_string());

    let observed_by = input
        .observed_by
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| OBSERVED_BY_UNKNOWN.to_string());
    let observed_role = input
        .observed_role
        .clone()
        .filter(|value| !value.trim().is_empty());

    // Label is whitelisted above — safe to interpolate. All string values are
    // escaped via escape_cypher_str before embedding in the query.
    let query = format!(
        concat!(
            "MERGE (n:{label} {{id: $id}}) ",
            "ON CREATE SET ",
            "n.created_at = $created_at, ",
            "n.source_membrane = $source_membrane, ",
            "n.provenance = $provenance, ",
            "n.confidence = $confidence, ",
            "n.validation_state = $validation_state, ",
            "n.observed_at = $observed_at, ",
            "n.last_confirmed_at = null, ",
            "n.claim_summary = $claim_summary, ",
            "n.observation_id = $observation_id, ",
            "n.packet_id = $packet_id, ",
            "n.observed_by = $observed_by, ",
            "n.observed_role = CASE $observed_role WHEN '' THEN null ELSE $observed_role END, ",
            "n.origin_engram_id = CASE $origin_engram_id WHEN '' THEN null ELSE $origin_engram_id END, ",
            "n.origin_trust = CASE WHEN $origin_trust < 0.0 THEN null ELSE $origin_trust END ",
            "ON MATCH SET ",
            "n.confidence = $confidence, ",
            "n.observation_id = $observation_id, ",
            "n.packet_id = $packet_id, ",
            "n.observed_by = $observed_by, ",
            "n.observed_role = CASE $observed_role WHEN '' THEN null ELSE $observed_role END ",
            "RETURN n.id AS id, n.validation_state AS validation_state",
        ),
        label = label
    );

    Ok(ObserveCypher {
        query,
        node_id: input.evidence.claim_ref.id.clone(),
        label: label.clone(),
        confidence: input.evidence.confidence as f64,
        source_membrane,
        provenance,
        observed_by,
        observed_role,
        validation_state: validation_state.to_string(),
        observed_at,
        created_at: now_iso.to_string(),
        claim_summary: input.evidence.claim_summary.clone(),
        observation_id: input.observation_id.clone(),
        packet_id: input.evidence.packet_id.clone(),
        origin_engram_id,
        origin_trust,
    })
}

/// Compile the optional living-cycle edges of a `life.observe` request.
///
/// Unknown rel_types are a hard error (callers should reject before the node
/// write). Compiled queries `MATCH` the target by id, so a missing target
/// simply produces no row — the provider reports it as `target_missing`
/// without failing the node write.
pub fn compile_observe_edges(input: &LifeObserveInput) -> Result<Vec<ObserveEdgeCypher>, String> {
    let label = &input.evidence.claim_ref.label;
    if !is_known_label(label) {
        return Err(format!("unknown Life Graph label: {label}"));
    }

    let mut compiled = Vec::with_capacity(input.edges.len());
    for edge in &input.edges {
        if !is_living_cycle_rel_type(&edge.rel_type) {
            return Err(format!(
                "unknown living-cycle rel_type: {} (expected one of {})",
                edge.rel_type,
                LIVING_CYCLE_REL_TYPES.join(", ")
            ));
        }
        if edge.target_id.trim().is_empty() {
            return Err("edge target_id must not be empty".to_string());
        }

        // Label and rel_type are both whitelisted above — safe to interpolate.
        // All values travel as bound parameters.
        let query = format!(
            concat!(
                "MATCH (n:{label} {{id: $id}}) ",
                "MATCH (t {{id: $target_id}}) ",
                "MERGE (n)-[r:{rel_type}]->(t) ",
                "ON CREATE SET ",
                "r.created_at = $created_at, ",
                "r.observation_id = $observation_id, ",
                "r.observed_by = $observed_by ",
                "RETURN t.id AS target_id",
            ),
            label = label,
            rel_type = edge.rel_type
        );

        compiled.push(ObserveEdgeCypher {
            query,
            rel_type: edge.rel_type.clone(),
            target_id: edge.target_id.clone(),
        });
    }

    Ok(compiled)
}

pub fn compile_commit(input: &LifeCommitInput, now_iso: &str) -> Result<CommitCypher, String> {
    input.evidence.validate().map_err(|e| e.to_string())?;
    let label = &input.evidence.claim_ref.label;
    if !is_known_label(label) {
        return Err(format!("unknown Life Graph label: {label}"));
    }

    // loop_status/resolution_note use the same CASE-empty-string-preserve
    // pattern as observed_role/origin_engram_id in compile_observe: an empty
    // string parameter means "leave the existing property untouched" rather
    // than overwriting it with null.
    let query = format!(
        concat!(
            "MERGE (n:{label} {{id: $id}}) ",
            "SET n.validation_state = 'confirmed', ",
            "n.last_confirmed_at = $confirmed_at, ",
            "n.confidence = $confidence, ",
            "n.claim_summary = $claim_summary, ",
            "n.packet_id = $packet_id, ",
            "n.status = CASE $loop_status WHEN '' THEN n.status ELSE $loop_status END, ",
            "n.resolved_at = CASE $loop_status WHEN '' THEN n.resolved_at ELSE $confirmed_at END, ",
            "n.resolution_note = CASE $resolution_note WHEN '' THEN n.resolution_note ELSE $resolution_note END ",
            "RETURN n.id AS id, n.validation_state AS validation_state, n.status AS status",
        ),
        label = label
    );

    Ok(CommitCypher {
        query,
        node_id: input.evidence.claim_ref.id.clone(),
        label: label.clone(),
        packet_id: input.evidence.packet_id.clone(),
        confidence: input.evidence.confidence as f64,
        claim_summary: input.evidence.claim_summary.clone(),
        confirmed_at: now_iso.to_string(),
        loop_status: input.loop_status.clone().unwrap_or_default(),
        resolution_note: input.resolution_note.clone().unwrap_or_default(),
    })
}

pub fn compile_conflict_handoff(
    handoff: &ConflictHandoff,
    now_iso: &str,
) -> Result<ConflictCypher, String> {
    handoff.validate().map_err(|e| e.to_string())?;
    let handoff_json = serde_json::to_string(handoff).map_err(|e| e.to_string())?;

    Ok(ConflictCypher {
        query: concat!(
            "MERGE (h:ConflictHandoff {id: $handoff_id}) ",
            "SET h.conflict_id = $conflict_id, ",
            "h.summary = $summary, ",
            "h.status = $status, ",
            "h.updated_at = $updated_at, ",
            "h.handoff_json = $handoff_json ",
            "RETURN h.id AS id, h.status AS status"
        )
        .to_string(),
        handoff_id: handoff.handoff_id.clone(),
        conflict_id: handoff.conflict_id.clone(),
        status: conflict_status_str(&handoff.status).to_string(),
        summary: handoff.summary.clone(),
        updated_at: now_iso.to_string(),
        resolution_summary: None,
        handoff_json,
    })
}

pub fn compile_resolve(input: &LifeResolveInput, now_iso: &str) -> Result<ConflictCypher, String> {
    let mut handoff = input.handoff.clone();
    handoff.status = ConflictHandoffStatus::Resolved;
    handoff.validate().map_err(|e| e.to_string())?;
    let handoff_json = serde_json::to_string(&handoff).map_err(|e| e.to_string())?;

    Ok(ConflictCypher {
        query: concat!(
            "MERGE (h:ConflictHandoff {id: $handoff_id}) ",
            "SET h.conflict_id = $conflict_id, ",
            "h.summary = $summary, ",
            "h.status = 'resolved', ",
            "h.resolution_summary = $resolution_summary, ",
            "h.resolved_at = $updated_at, ",
            "h.updated_at = $updated_at, ",
            "h.handoff_json = $handoff_json ",
            "RETURN h.id AS id, h.status AS status"
        )
        .to_string(),
        handoff_id: handoff.handoff_id.clone(),
        conflict_id: handoff.conflict_id.clone(),
        status: "resolved".to_string(),
        summary: handoff.summary.clone(),
        updated_at: now_iso.to_string(),
        resolution_summary: Some(input.resolution_summary.clone()),
        handoff_json,
    })
}

pub fn compile_patch_proposal(
    input: &LifePatchProposalInput,
    now_iso: &str,
) -> Result<PatchProposalCypher, String> {
    compile_patch_proposal_with_status(input, now_iso, PATCH_STATUS_PROPOSED)
}

/// Patch node lifecycle statuses (Autopoiesis Slice A2).
pub const PATCH_STATUS_PROPOSED: &str = "proposed";
/// The patch carries ready-to-apply edge specs and a hotel audit id; it is
/// waiting for `life.patch.apply` (operator confirmation).
pub const PATCH_STATUS_AWAITING_CONFIRMATION: &str = "awaiting_confirmation";
/// The embedded edge specs were executed (auto or via confirmation).
pub const PATCH_STATUS_APPLIED: &str = "applied";
/// The operator rejected the patch; nothing was written.
pub const PATCH_STATUS_REJECTED: &str = "rejected";

/// [`compile_patch_proposal`] with an explicit lifecycle status. Slice A2
/// files SafeAutoUpdate patches as `awaiting_confirmation` (ConfirmFirst
/// posture) or `applied` (AutoWithAudit) instead of plain `proposed`.
pub fn compile_patch_proposal_with_status(
    input: &LifePatchProposalInput,
    now_iso: &str,
    status: &str,
) -> Result<PatchProposalCypher, String> {
    if status.trim().is_empty() {
        return Err("patch status must not be empty".to_string());
    }
    let patch_json = serde_json::to_string(input).map_err(|e| e.to_string())?;
    let label = patch_label(&input.patch_kind);

    let query = format!(
        concat!(
            "MERGE (p:{label} {{id: $patch_id}}) ",
            "SET p.patch_kind = $patch_kind, ",
            "p.summary = $summary, ",
            "p.rationale = $rationale, ",
            "p.risk = $risk, ",
            "p.status = $status, ",
            "p.proposed_at = $proposed_at, ",
            "p.patch_json = $patch_json ",
            "RETURN p.id AS id, p.status AS status"
        ),
        label = label
    );

    Ok(PatchProposalCypher {
        query,
        patch_id: input.patch_id.clone(),
        label: label.to_string(),
        patch_kind: patch_kind_str(&input.patch_kind).to_string(),
        summary: input.summary.clone(),
        rationale: input.rationale.clone(),
        risk: format!("{:?}", input.risk).to_ascii_lowercase(),
        status: status.to_string(),
        proposed_at: now_iso.to_string(),
        patch_json,
    })
}

/// One compiled feedback bridge-edge MERGE (Autopoiesis Slice A2, lane
/// `graph.bridge_edges`).
///
/// Both endpoints are `MATCH`ed by id, so a missing node writes nothing —
/// the caller reports the miss instead of failing. The MERGE is idempotent:
/// re-applying the same spec never duplicates the edge, and `ON CREATE SET`
/// preserves the original provenance stamp.
#[derive(Debug)]
pub struct BridgeEdgeCypher {
    pub query: String,
    pub from_id: String,
    pub to_id: String,
    pub created_by: String,
    pub feedback_signal_id: String,
    pub created_at: String,
}

pub fn compile_feedback_bridge_edge(spec: &FeedbackEdgeSpec) -> Result<BridgeEdgeCypher, String> {
    if !is_living_cycle_rel_type(&spec.rel_type) {
        return Err(format!(
            "unknown living-cycle rel_type: {} (expected one of {})",
            spec.rel_type,
            LIVING_CYCLE_REL_TYPES.join(", ")
        ));
    }
    if spec.from_id.trim().is_empty() || spec.to_id.trim().is_empty() {
        return Err("bridge edge endpoints must not be empty".to_string());
    }
    if spec.from_id == spec.to_id {
        return Err("bridge edge must not be a self-edge".to_string());
    }

    // rel_type is whitelisted above — safe to interpolate. All values travel
    // as bound parameters.
    let query = format!(
        concat!(
            "MATCH (a {{id: $from_id}}) ",
            "MATCH (b {{id: $to_id}}) ",
            "MERGE (a)-[r:{rel_type}]->(b) ",
            "ON CREATE SET ",
            "r.created_at = $created_at, ",
            "r.created_by = $created_by, ",
            "r.feedback_signal_id = $feedback_signal_id ",
            "RETURN b.id AS target_id",
        ),
        rel_type = spec.rel_type
    );

    Ok(BridgeEdgeCypher {
        query,
        from_id: spec.from_id.clone(),
        to_id: spec.to_id.clone(),
        created_by: spec.created_by.clone(),
        feedback_signal_id: spec.feedback_signal_id.clone(),
        created_at: spec.created_at.clone(),
    })
}

/// Lookup a patch node by id regardless of patch label — returns its current
/// status and the embedded `patch_json` (which carries the edge specs and
/// the hotel audit id). Used by `life.patch.apply`.
pub fn patch_lookup_query() -> &'static str {
    "MATCH (p {id: $patch_id}) WHERE p.patch_json IS NOT NULL \
     RETURN p.status AS status, p.patch_json AS patch_json LIMIT 1"
}

/// Update a patch node's lifecycle status. Used by `life.patch.apply` to
/// move `awaiting_confirmation` → `applied` / `rejected`.
pub fn patch_status_update_query() -> &'static str {
    "MATCH (p {id: $patch_id}) WHERE p.patch_json IS NOT NULL \
     SET p.status = $status, p.status_updated_at = $updated_at \
     RETURN p.id AS id, p.status AS status"
}

pub fn compile_recall_feedback(
    input: &RetrievalFeedbackInput,
    growth_evaluation: &serde_json::Value,
    now_iso: &str,
) -> Result<RecallFeedbackCypher, String> {
    input.validate().map_err(|e| e.to_string())?;
    let feedback_json = serde_json::to_string(input).map_err(|e| e.to_string())?;
    let evaluation_json = serde_json::to_string(growth_evaluation).map_err(|e| e.to_string())?;
    let connectivity_ratio = input.connectivity_ratio().map(f64::from);

    Ok(RecallFeedbackCypher {
        query: concat!(
            "MERGE (s:Signal {id: $feedback_id}) ",
            "SET s.signal_type = 'life.recall.feedback', ",
            "s.packet_id = $packet_id, ",
            "s.rating = $rating, ",
            "s.query_summary = $query_summary, ",
            "s.note = $note, ",
            "s.candidate_count = $candidate_count, ",
            "s.connected_candidate_count = $connected_candidate_count, ",
            "s.connectivity_ratio = $connectivity_ratio, ",
            "s.feedback_json = $feedback_json, ",
            "s.growth_evaluation_json = $evaluation_json, ",
            "s.source_membrane = 'agent:memorygraphrag', ",
            "s.provenance = 'runtime_observation', ",
            "s.confidence = 1.0, ",
            "s.validation_state = 'confirmed', ",
            "s.observed_at = $observed_at, ",
            "s.last_confirmed_at = $observed_at ",
            "RETURN s.id AS id, s.rating AS rating"
        )
        .to_string(),
        feedback_id: input.feedback_id.clone(),
        packet_id: input.packet_id.clone(),
        rating: feedback_rating_str(&input.rating).to_string(),
        query_summary: input.query_summary.clone().unwrap_or_default(),
        note: input.note.clone().unwrap_or_default(),
        candidate_count: input.candidate_count as i64,
        connected_candidate_count: input.connected_candidate_count as i64,
        connectivity_ratio,
        feedback_json,
        evaluation_json,
        observed_at: now_iso.to_string(),
    })
}

fn source_kind_to_provenance(kind: &SourceKind) -> String {
    match kind {
        SourceKind::OperatorConfirmation => "operator_confirmed",
        SourceKind::MembraneEvent => "transcript",
        // Preserved verbatim so Muninn-origin nodes remain distinguishable in
        // the graph — do NOT collapse this into "agent_inferred".
        SourceKind::MuninnEngram => "muninn_engram",
        SourceKind::GraphPassage => "agent_inferred",
        SourceKind::ImportedRecord => "calendar",
        SourceKind::AgentInference => "agent_inferred",
        SourceKind::RuntimeObservation => "transcript",
    }
    .to_string()
}

fn validation_state_str(state: &ValidationState) -> &'static str {
    match state {
        ValidationState::Inferred => "inferred",
        ValidationState::Proposed => "proposed",
        ValidationState::Confirmed => "confirmed",
        ValidationState::Retired => "retired",
        ValidationState::Conflicted => "conflicted",
    }
}

pub fn is_known_label(label: &str) -> bool {
    KNOWN_LABELS.contains(&label)
}

fn conflict_status_str(status: &ConflictHandoffStatus) -> &'static str {
    match status {
        ConflictHandoffStatus::Open => "open",
        ConflictHandoffStatus::SentToMuninn => "sent_to_muninn",
        ConflictHandoffStatus::AwaitingOperator => "awaiting_operator",
        ConflictHandoffStatus::Resolved => "resolved",
        ConflictHandoffStatus::ClosedNoAction => "closed_no_action",
    }
}

fn patch_label(kind: &PatchKind) -> &'static str {
    match kind {
        PatchKind::SchemaPatch => "SchemaPatch",
        PatchKind::SkillPatch => "SkillPatch",
        PatchKind::ToolPatch => "ToolPatch",
        PatchKind::AttentionPatch => "AttentionPatch",
        PatchKind::SystemPatch => "SystemPatch",
    }
}

fn patch_kind_str(kind: &PatchKind) -> &'static str {
    match kind {
        PatchKind::SchemaPatch => "schema_patch",
        PatchKind::SkillPatch => "skill_patch",
        PatchKind::ToolPatch => "tool_patch",
        PatchKind::AttentionPatch => "attention_patch",
        PatchKind::SystemPatch => "system_patch",
    }
}

fn feedback_rating_str(rating: &RetrievalFeedbackRating) -> &'static str {
    match rating {
        RetrievalFeedbackRating::Useful => "useful",
        RetrievalFeedbackRating::Stale => "stale",
        RetrievalFeedbackRating::Missing => "missing",
        RetrievalFeedbackRating::Noisy => "noisy",
        RetrievalFeedbackRating::Overconfident => "overconfident",
        RetrievalFeedbackRating::Disconnected => "disconnected",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AdjudicationStatus, EvidencePacket, GraphRecordRef, LifeObserveInput, ReliabilityBasis,
        RetrievalFeedbackInput, RetrievalFeedbackRating, SourceKind, SourceRef, SourceReliability,
        ValidationState,
    };

    fn minimal_observe_input(label: &str) -> LifeObserveInput {
        LifeObserveInput {
            observation_id: "obs-001".to_string(),
            evidence: EvidencePacket {
                packet_id: "pkt-001".to_string(),
                claim_ref: GraphRecordRef {
                    id: "signal-abc".to_string(),
                    label: label.to_string(),
                    datasource: None,
                },
                claim_summary: "test signal".to_string(),
                source_refs: vec![SourceRef {
                    source_id: "membrane:telegram".to_string(),
                    source_kind: SourceKind::MembraneEvent,
                    reliability: SourceReliability {
                        score: 0.9,
                        basis: ReliabilityBasis::DirectObservation,
                    },
                    uri: None,
                    captured_at: None,
                }],
                passage_refs: vec![],
                confidence: 0.8,
                validation_state: ValidationState::Proposed,
                observed_at: Some("2026-06-04T00:00:00Z".to_string()),
                valid_time_range: None,
                source_reliability: 0.9,
                conflict_ids: vec![],
                adjudication_status: AdjudicationStatus::NotNeeded,
                metadata: serde_json::Value::Null,
            },
            proposed_graph_refs: vec![],
            observed_by: None,
            observed_role: None,
            edges: vec![],
        }
    }

    #[test]
    fn compile_observe_signal_label_ok() {
        let input = minimal_observe_input("Signal");
        let compiled = compile_observe(&input, "2026-06-04T12:00:00Z").unwrap();

        assert_eq!(compiled.label, "Signal");
        assert_eq!(compiled.node_id, "signal-abc");
        assert_eq!(compiled.observation_id, "obs-001");
        assert_eq!(compiled.packet_id, "pkt-001");
        assert!((compiled.confidence - 0.8_f64).abs() < 1e-5);
        assert_eq!(compiled.validation_state, "proposed");
        assert_eq!(compiled.source_membrane, "membrane:telegram");
        assert_eq!(compiled.provenance, "transcript");
        assert_eq!(compiled.observed_at, "2026-06-04T00:00:00Z");
        assert!(compiled.query.contains("MERGE (n:Signal {id: $id})"));
        assert!(compiled.query.contains("ON CREATE SET"));
        assert!(compiled.query.contains("ON MATCH SET"));
        assert!(compiled.query.contains("RETURN n.id AS id"));
    }

    #[test]
    fn compile_observe_unknown_label_is_rejected() {
        let mut input = minimal_observe_input("Signal");
        input.evidence.claim_ref.label = "Gadget".to_string();
        let err = compile_observe(&input, "2026-06-04T12:00:00Z").unwrap_err();
        assert!(err.contains("Gadget"));
    }

    #[test]
    fn compile_observe_defaults_observed_by_for_legacy_callers() {
        let input = minimal_observe_input("Signal");
        let compiled = compile_observe(&input, "2026-06-04T12:00:00Z").unwrap();

        assert_eq!(compiled.observed_by, OBSERVED_BY_UNKNOWN);
        assert_eq!(compiled.observed_role, None);
        assert!(compiled.query.contains("n.observed_by = $observed_by"));
        assert!(compiled.query.contains("n.observed_role = CASE"));
    }

    #[test]
    fn compile_observe_carries_agent_provenance_distinct_from_membrane() {
        let mut input = minimal_observe_input("OpenLoop");
        input.evidence.claim_ref.id = "life:open-loop:prov".to_string();
        input.observed_by = Some("agent-astrid-01".to_string());
        input.observed_role = Some("librarian".to_string());
        let compiled = compile_observe(&input, "2026-06-04T12:00:00Z").unwrap();

        assert_eq!(compiled.observed_by, "agent-astrid-01");
        assert_eq!(compiled.observed_role.as_deref(), Some("librarian"));
        // Transport stays what it truly was.
        assert_eq!(compiled.source_membrane, "membrane:telegram");
    }

    #[test]
    fn compile_observe_blank_observed_by_falls_back_to_unknown() {
        let mut input = minimal_observe_input("Signal");
        input.observed_by = Some("   ".to_string());
        input.observed_role = Some(String::new());
        let compiled = compile_observe(&input, "2026-06-04T12:00:00Z").unwrap();

        assert_eq!(compiled.observed_by, OBSERVED_BY_UNKNOWN);
        assert_eq!(compiled.observed_role, None);
    }

    #[test]
    fn compile_observe_muninn_source_preserves_origin() {
        let mut input = minimal_observe_input("Signal");
        input.evidence.claim_ref.id = "life:signal:muninn-origin".to_string();
        input.evidence.source_refs = vec![SourceRef {
            source_id: "muninn:01ABCDEF".to_string(),
            source_kind: SourceKind::MuninnEngram,
            reliability: SourceReliability {
                score: 0.85,
                basis: ReliabilityBasis::MuninnTrust,
            },
            uri: None,
            captured_at: None,
        }];
        let compiled = compile_observe(&input, "2026-07-07T00:00:00Z").unwrap();

        assert_eq!(compiled.provenance, "muninn_engram");
        assert_eq!(
            compiled.origin_engram_id.as_deref(),
            Some("muninn:01ABCDEF")
        );
        assert!((compiled.origin_trust.unwrap() - 0.85).abs() < 1e-5);
        assert!(compiled.query.contains(
            "n.origin_engram_id = CASE $origin_engram_id WHEN '' THEN null ELSE $origin_engram_id END"
        ));
        assert!(compiled.query.contains(
            "n.origin_trust = CASE WHEN $origin_trust < 0.0 THEN null ELSE $origin_trust END"
        ));
    }

    #[test]
    fn compile_observe_muninn_origin_found_behind_other_sources() {
        let mut input = minimal_observe_input("OpenLoop");
        input.evidence.claim_ref.id = "life:open-loop:mixed-sources".to_string();
        input.evidence.source_refs.push(SourceRef {
            source_id: "muninn:01SECOND".to_string(),
            source_kind: SourceKind::MuninnEngram,
            reliability: SourceReliability {
                score: 0.7,
                basis: ReliabilityBasis::MuninnTrust,
            },
            uri: None,
            captured_at: None,
        });
        let compiled = compile_observe(&input, "2026-07-07T00:00:00Z").unwrap();

        // First source ref still drives transport + provenance...
        assert_eq!(compiled.source_membrane, "membrane:telegram");
        assert_eq!(compiled.provenance, "transcript");
        // ...but the Muninn origin is preserved from the later source ref.
        assert_eq!(
            compiled.origin_engram_id.as_deref(),
            Some("muninn:01SECOND")
        );
        assert!((compiled.origin_trust.unwrap() - 0.7).abs() < 1e-5);
    }

    #[test]
    fn compile_observe_non_muninn_source_leaves_origin_null() {
        let input = minimal_observe_input("Signal");
        let compiled = compile_observe(&input, "2026-07-07T00:00:00Z").unwrap();

        assert_eq!(compiled.origin_engram_id, None);
        assert_eq!(compiled.origin_trust, None);
        assert_eq!(compiled.provenance, "transcript");
    }

    #[test]
    fn compile_observe_old_payload_without_muninn_fields_still_compiles() {
        // A wire payload predating Muninn provenance preservation: no new
        // input fields exist, so an old-style life.observe body must
        // deserialize and compile with null origin properties.
        let payload = serde_json::json!({
            "observation_id": "obs-legacy",
            "evidence": {
                "packet_id": "pkt-legacy",
                "claim_ref": { "id": "signal-legacy", "label": "Signal" },
                "claim_summary": "legacy signal",
                "source_refs": [{
                    "source_id": "membrane:telegram",
                    "source_kind": "membrane_event",
                    "reliability": { "score": 0.9, "basis": "direct_observation" }
                }],
                "passage_refs": [],
                "confidence": 0.8,
                "validation_state": "proposed",
                "source_reliability": 0.9,
                "conflict_ids": [],
                "adjudication_status": "not_needed",
                "metadata": null
            },
            "proposed_graph_refs": []
        });
        let input: LifeObserveInput = serde_json::from_value(payload).unwrap();
        let compiled = compile_observe(&input, "2026-07-07T00:00:00Z").unwrap();

        assert_eq!(compiled.origin_engram_id, None);
        assert_eq!(compiled.origin_trust, None);
    }

    #[test]
    fn compile_observe_edges_merges_living_cycle_rel_types() {
        let mut input = minimal_observe_input("Goal");
        input.evidence.claim_ref.id = "life:goal:row-weekly".to_string();
        input.observed_by = Some("agent-beacon-01".to_string());
        input.edges = vec![
            crate::ObserveEdge {
                rel_type: "OWNS".into(),
                target_id: "life:role:chief-of-staff".into(),
            },
            crate::ObserveEdge {
                rel_type: "RELATES_TO".into(),
                target_id: "life:role:musician".into(),
            },
        ];

        let compiled = compile_observe_edges(&input).unwrap();
        assert_eq!(compiled.len(), 2);
        assert_eq!(compiled[0].rel_type, "OWNS");
        assert_eq!(compiled[0].target_id, "life:role:chief-of-staff");
        assert!(compiled[0].query.contains("MATCH (n:Goal {id: $id})"));
        assert!(compiled[0].query.contains("MATCH (t {id: $target_id})"));
        assert!(compiled[0].query.contains("MERGE (n)-[r:OWNS]->(t)"));
        assert!(compiled[0].query.contains("r.observed_by = $observed_by"));
        assert!(compiled[1].query.contains("MERGE (n)-[r:RELATES_TO]->(t)"));
    }

    #[test]
    fn compile_observe_edges_rejects_unknown_rel_type() {
        let mut input = minimal_observe_input("Goal");
        input.edges = vec![crate::ObserveEdge {
            rel_type: "DESTROYS".into(),
            target_id: "life:role:musician".into(),
        }];

        let err = compile_observe_edges(&input).unwrap_err();
        assert!(err.contains("DESTROYS"));
        assert!(err.contains("OWNS"));
    }

    #[test]
    fn compile_observe_edges_rejects_empty_target() {
        let mut input = minimal_observe_input("Goal");
        input.edges = vec![crate::ObserveEdge {
            rel_type: "SETS".into(),
            target_id: "  ".into(),
        }];

        let err = compile_observe_edges(&input).unwrap_err();
        assert!(err.contains("target_id"));
    }

    #[test]
    fn living_cycle_rel_type_set_is_the_approved_vocabulary() {
        assert_eq!(
            LIVING_CYCLE_REL_TYPES,
            &["OWNS", "SHAPES", "SETS", "SPAWNS", "RELATES_TO"]
        );
        assert!(is_living_cycle_rel_type("SHAPES"));
        assert!(!is_living_cycle_rel_type("owns"));
    }

    #[test]
    fn compile_observe_all_known_labels_accepted() {
        for label in KNOWN_LABELS {
            let mut input = minimal_observe_input(label);
            input.evidence.claim_ref.id = format!("{}-test-id", label.to_lowercase());
            let result = compile_observe(&input, "2026-06-04T12:00:00Z");
            assert!(result.is_ok(), "label {label} should be accepted");
        }
    }

    #[test]
    fn compile_observe_uses_now_when_observed_at_absent() {
        let mut input = minimal_observe_input("Goal");
        input.evidence.claim_ref.id = "goal-xyz".to_string();
        input.evidence.observed_at = None;
        let compiled = compile_observe(&input, "2026-06-04T09:00:00Z").unwrap();
        assert_eq!(compiled.observed_at, "2026-06-04T09:00:00Z");
        assert_eq!(compiled.created_at, "2026-06-04T09:00:00Z");
    }

    #[test]
    fn compile_observe_defaults_source_membrane_when_no_source_refs() {
        let mut input = minimal_observe_input("Event");
        input.evidence.claim_ref.id = "event-xyz".to_string();
        input.evidence.source_refs = vec![];
        input.evidence.passage_refs = vec![crate::PassageRef {
            passage_id: "p-1".to_string(),
            source_ref_id: Some("membrane:calendar".to_string()),
            excerpt_hash: None,
            muninn_engram_id: None,
            graph_node_id: None,
        }];
        let compiled = compile_observe(&input, "2026-06-04T09:00:00Z").unwrap();
        assert_eq!(compiled.source_membrane, "membrane:calendar");
    }

    #[test]
    fn compile_commit_confirms_known_life_graph_label() {
        let mut evidence = minimal_observe_input("OpenLoop").evidence;
        evidence.validation_state = ValidationState::Confirmed;
        let compiled = compile_commit(
            &crate::LifeCommitInput {
                evidence,
                operator_approved: false,
                loop_status: None,
                resolution_note: None,
            },
            "2026-06-05T09:00:00Z",
        )
        .unwrap();

        assert_eq!(compiled.label, "OpenLoop");
        assert_eq!(compiled.confirmed_at, "2026-06-05T09:00:00Z");
        assert!(compiled.query.contains("MERGE (n:OpenLoop {id: $id})"));
        assert!(compiled.query.contains("n.validation_state = 'confirmed'"));
        // No loop_status supplied — the sentinel is empty, so the query must
        // preserve the existing n.status rather than clobbering it with null.
        assert_eq!(compiled.loop_status, "");
        assert!(
            compiled
                .query
                .contains("CASE $loop_status WHEN '' THEN n.status")
        );
    }

    #[test]
    fn compile_commit_with_loop_status_closes_the_loop() {
        // This is the "Confirm for both. Finished my YPT." case: the operator
        // reports the underlying loop done in the same breath as confirming
        // it, so life.commit must be able to close it, not just promote
        // validation_state. See bug: Beacon re-recorded stale "paused"
        // content instead of resolving the loop because no tool set status.
        let mut evidence = minimal_observe_input("OpenLoop").evidence;
        evidence.validation_state = ValidationState::Confirmed;
        evidence.claim_summary = "Completed YPT (Youth Protection Training).".to_string();
        let compiled = compile_commit(
            &crate::LifeCommitInput {
                evidence,
                operator_approved: true,
                loop_status: Some("resolved".to_string()),
                resolution_note: Some("operator reported complete 2026-07-09".to_string()),
            },
            "2026-07-09T11:08:00Z",
        )
        .unwrap();

        assert_eq!(compiled.loop_status, "resolved");
        assert_eq!(
            compiled.resolution_note,
            "operator reported complete 2026-07-09"
        );
        assert!(compiled.query.contains("n.resolved_at = CASE $loop_status"));
        assert!(
            compiled
                .query
                .contains("n.resolution_note = CASE $resolution_note")
        );
    }

    #[test]
    fn compile_conflict_handoff_stores_open_handoff() {
        let handoff = ConflictHandoff {
            handoff_id: "handoff:1".into(),
            conflict_id: "conflict:1".into(),
            finding_type: crate::ConflictFindingType::DirectContradiction,
            summary: "Graph and Muninn disagree.".into(),
            graph_fact_refs: vec![GraphRecordRef {
                id: "life:open_loop:1".into(),
                label: "OpenLoop".into(),
                datasource: None,
            }],
            evidence_packets: vec![minimal_observe_input("OpenLoop").evidence],
            muninn_engram_ids: vec![],
            recommended_owner: crate::HandoffOwner::DataMemoryGraphRag,
            requested_muninn_action: crate::MuninnRequestedAction::None,
            risk: crate::ConflictRisk::Low,
            requires_operator: false,
            status: ConflictHandoffStatus::Open,
            metadata: serde_json::json!({}),
        };

        let compiled = compile_conflict_handoff(&handoff, "2026-06-05T09:00:00Z").unwrap();
        assert_eq!(compiled.status, "open");
        assert_eq!(compiled.conflict_id, "conflict:1");
        assert!(compiled.query.contains("MERGE (h:ConflictHandoff"));
        assert!(compiled.handoff_json.contains("direct_contradiction"));
    }

    #[test]
    fn compile_resolve_marks_handoff_resolved() {
        let handoff = ConflictHandoff {
            handoff_id: "handoff:2".into(),
            conflict_id: "conflict:2".into(),
            finding_type: crate::ConflictFindingType::TemporalConflict,
            summary: "Old and new commitment dates conflict.".into(),
            graph_fact_refs: vec![GraphRecordRef {
                id: "life:commitment:1".into(),
                label: "Commitment".into(),
                datasource: None,
            }],
            evidence_packets: vec![minimal_observe_input("Commitment").evidence],
            muninn_engram_ids: vec![],
            recommended_owner: crate::HandoffOwner::DataMemoryGraphRag,
            requested_muninn_action: crate::MuninnRequestedAction::None,
            risk: crate::ConflictRisk::Low,
            requires_operator: false,
            status: ConflictHandoffStatus::Open,
            metadata: serde_json::json!({}),
        };

        let compiled = compile_resolve(
            &crate::LifeResolveInput {
                handoff,
                resolution_summary: "Use the newer date.".into(),
                operator_approved: false,
            },
            "2026-06-05T09:00:00Z",
        )
        .unwrap();

        assert_eq!(compiled.status, "resolved");
        assert_eq!(
            compiled.resolution_summary.as_deref(),
            Some("Use the newer date.")
        );
        assert!(compiled.query.contains("h.status = 'resolved'"));
    }

    #[test]
    fn compile_patch_proposal_uses_patch_label() {
        let compiled = compile_patch_proposal(
            &crate::LifePatchProposalInput {
                patch_id: "patch:attention:1".into(),
                patch_kind: PatchKind::AttentionPatch,
                summary: "Dampen a noisy nudge.".into(),
                rationale: "Recent friction suggests timing is off.".into(),
                evidence_packets: vec![minimal_observe_input("Signal").evidence],
                risk: crate::PatchRisk::Medium,
                operator_approved: false,
                edge_specs: vec![],
                autonomy_audit_id: None,
            },
            "2026-06-05T09:00:00Z",
        )
        .unwrap();

        assert_eq!(compiled.label, "AttentionPatch");
        assert_eq!(compiled.patch_kind, "attention_patch");
        assert!(compiled.query.contains("MERGE (p:AttentionPatch"));
        assert!(compiled.patch_json.contains("Dampen"));
    }

    #[test]
    fn compile_recall_feedback_writes_signal_with_growth_evaluation() {
        let feedback = RetrievalFeedbackInput {
            feedback_id: "feedback:recall:1".into(),
            packet_id: "packet:recall:1".into(),
            query_summary: Some("Re-enter LifeGraph work".into()),
            rating: RetrievalFeedbackRating::Disconnected,
            note: Some("Good facts, but they were not connected to the active project.".into()),
            candidate_count: 4,
            connected_candidate_count: 1,
            missing_context_refs: vec![],
            noisy_node_refs: vec![],
            stale_node_refs: vec![],
            evidence_packets: vec![minimal_observe_input("Signal").evidence],
            query_context_ref: None,
            connected_candidate_refs: vec![],
        };
        let growth_evaluation = serde_json::json!({
            "disposition": "apply_with_audit",
            "rationale": ["low connected-candidate ratio"]
        });

        let compiled =
            compile_recall_feedback(&feedback, &growth_evaluation, "2026-06-22T10:00:00Z").unwrap();

        assert_eq!(compiled.feedback_id, "feedback:recall:1");
        assert_eq!(compiled.rating, "disconnected");
        assert_eq!(compiled.connectivity_ratio, Some(0.25));
        assert!(compiled.query.contains("MERGE (s:Signal"));
        assert!(compiled.feedback_json.contains("packet:recall:1"));
        assert!(compiled.evaluation_json.contains("apply_with_audit"));
    }

    // ── Feedback bridge edges (Autopoiesis Slice A2) ──────────────────────────

    fn bridge_spec() -> FeedbackEdgeSpec {
        FeedbackEdgeSpec {
            from_id: "life:open_loop:anchor".into(),
            to_id: "life:project:phi".into(),
            rel_type: "RELATES_TO".into(),
            created_by: crate::FEEDBACK_EDGE_CREATED_BY.into(),
            feedback_signal_id: "feedback:recall:a2".into(),
            created_at: "2026-07-07T00:00:00Z".into(),
        }
    }

    #[test]
    fn compile_feedback_bridge_edge_is_idempotent_merge_with_provenance() {
        let compiled = compile_feedback_bridge_edge(&bridge_spec()).unwrap();

        // Idempotent MERGE between MATCHed endpoints: a re-apply never
        // duplicates the edge, and a missing endpoint writes nothing.
        assert!(compiled.query.contains("MATCH (a {id: $from_id})"));
        assert!(compiled.query.contains("MATCH (b {id: $to_id})"));
        assert!(compiled.query.contains("MERGE (a)-[r:RELATES_TO]->(b)"));
        // ON CREATE SET keeps the original provenance stamp on re-merge.
        assert!(compiled.query.contains("ON CREATE SET"));
        assert!(compiled.query.contains("r.created_by = $created_by"));
        assert!(
            compiled
                .query
                .contains("r.feedback_signal_id = $feedback_signal_id")
        );
        assert!(compiled.query.contains("r.created_at = $created_at"));
        assert!(compiled.query.contains("RETURN b.id AS target_id"));
        // Values travel as bound params, never interpolated.
        assert!(!compiled.query.contains("life:open_loop:anchor"));
        assert_eq!(compiled.created_by, "feedback-to-action");
        assert_eq!(compiled.feedback_signal_id, "feedback:recall:a2");
    }

    #[test]
    fn compile_feedback_bridge_edge_rejects_bad_specs() {
        let mut spec = bridge_spec();
        spec.rel_type = "CAUSED".into();
        assert!(
            compile_feedback_bridge_edge(&spec)
                .unwrap_err()
                .contains("unknown living-cycle rel_type")
        );

        let mut spec = bridge_spec();
        spec.to_id = "  ".into();
        assert!(
            compile_feedback_bridge_edge(&spec)
                .unwrap_err()
                .contains("must not be empty")
        );

        let mut spec = bridge_spec();
        spec.to_id = spec.from_id.clone();
        assert!(
            compile_feedback_bridge_edge(&spec)
                .unwrap_err()
                .contains("self-edge")
        );
    }

    #[test]
    fn compile_patch_proposal_with_status_embeds_edge_specs() {
        let input = crate::LifePatchProposalInput {
            patch_id: "patch:recall-feedback:a2".into(),
            patch_kind: PatchKind::SystemPatch,
            summary: "Bridge disconnected recall candidates.".into(),
            rationale: "Feedback carried an unambiguous structural remedy.".into(),
            evidence_packets: vec![minimal_observe_input("Signal").evidence],
            risk: crate::PatchRisk::Low,
            operator_approved: false,
            edge_specs: vec![bridge_spec()],
            autonomy_audit_id: Some("autonomy:graph.bridge_edges:abc".into()),
        };

        let compiled = compile_patch_proposal_with_status(
            &input,
            "2026-07-07T00:00:00Z",
            "awaiting_confirmation",
        )
        .unwrap();
        assert_eq!(compiled.status, PATCH_STATUS_AWAITING_CONFIRMATION);
        // The ready-to-apply spec and audit anchor ride in patch_json — the
        // patch node alone is enough for life.patch.apply to execute later.
        assert!(compiled.patch_json.contains("life:project:phi"));
        assert!(compiled.patch_json.contains("RELATES_TO"));
        assert!(
            compiled
                .patch_json
                .contains("autonomy:graph.bridge_edges:abc")
        );

        // Default entry point still files plain proposals.
        let compiled = compile_patch_proposal(&input, "2026-07-07T00:00:00Z").unwrap();
        assert_eq!(compiled.status, PATCH_STATUS_PROPOSED);

        assert!(
            compile_patch_proposal_with_status(&input, "2026-07-07T00:00:00Z", " ")
                .unwrap_err()
                .contains("status")
        );
    }

    #[test]
    fn patch_apply_queries_target_patch_nodes_only() {
        assert!(patch_lookup_query().contains("p.patch_json IS NOT NULL"));
        assert!(patch_lookup_query().contains("$patch_id"));
        assert!(patch_status_update_query().contains("SET p.status = $status"));
        assert!(patch_status_update_query().contains("p.status_updated_at = $updated_at"));
    }
}
