//! Central LifeGraph ontology — the single authoritative vocabulary for
//! node labels, lifecycle states, property conventions, and the named
//! deterministic maintenance queries built from them.
//!
//! Everything that used to be scattered lore (which statuses are terminal,
//! which property a closure lands on, which fields carry dates) is declared
//! here ONCE and consumed by:
//!   - the recall exclusion filter (`projection::VectorHit::is_retired`)
//!   - the raw recall fallback cypher (`provider::raw_recall_fallback_cypher`)
//!   - the deterministic list surface (`life.list`, `provider::handle_list`)
//!   - the agent-facing vocabulary document (`life.ontology`)
//!
//! Steward skills must reference this vocabulary through the `life.ontology`
//! tool rather than restating it in prompt text, so schema knowledge cannot
//! drift per-agent. The 2026-08-20 stale-brief incident was exactly this
//! drift: closures written to a non-canonical property (`loop_status`) and a
//! terminal value (`resolved`) missing from the exclusion set.

use serde_json::{Value, json};

/// Bump when the vocabulary, conventions, or named query set changes shape.
pub const ONTOLOGY_VERSION: &str = "2";

// ── Lifecycle vocabulary ─────────────────────────────────────────────────────

/// Status values that mean a node's lifecycle is OVER. Terminal nodes are
/// excluded from recall, briefs, and default `life.list` output.
///
/// `resolved` is what `life.commit`/`life.resolve` write on loop closure —
/// it is as terminal as `done` (regression guard: PR #434).
pub const TERMINAL_STATUSES: &[&str] = &["retired", "done", "fulfilled", "abandoned", "resolved"];

/// Properties a lifecycle status may live on, in priority order.
///
/// `status` is canonical. `loop_status` is a legacy alias observed on
/// production nodes (closures written by early agent flows); readers must
/// honor both, writers must only ever write `status`.
pub const STATUS_PROPERTIES: &[&str] = &["status", "loop_status"];

/// Validation states a node moves through (provenance axis, orthogonal to
/// lifecycle status). `retired` is terminal on this axis too.
pub const VALIDATION_STATES: &[&str] =
    &["proposed", "inferred", "confirmed", "conflicted", "retired"];

/// All node labels the LifeGraph recognizes (union of the semantic spaces —
/// keep in sync with `projection::labels_for_space`).
pub const NODE_LABELS: &[&str] = &[
    "Event",
    "Signal",
    "OpenLoop",
    "Goal",
    "System",
    "Habit",
    "Project",
    "Routine",
    "NextAction",
    "GrowthHypothesis",
    "GrowthExperiment",
    "DriftFinding",
    "CapabilityPatch",
    "SkillPatch",
    "ToolPatch",
    "SchemaPatch",
    "AttentionPatch",
    "SystemPatch",
    "Role",
    "Aspiration",
    "Person",
    "Value",
    "Preference",
    "Concern",
    "Commitment",
    "Decision",
    "Place",
    "Trip",
    "Appointment",
    "Subscription",
    "Asset",
    "CreativeWork",
    "Moment",
];

/// Structured date/time properties, in the order a "best date" coalesce
/// should consult them. All ISO 8601 strings.
pub const DATE_PROPERTIES: &[&str] = &["due_at", "starts_at", "occurs_at", "ends_at"];

pub fn is_known_label(label: &str) -> bool {
    NODE_LABELS.contains(&label)
}

pub fn is_terminal_status(status: &str) -> bool {
    TERMINAL_STATUSES.contains(&status)
}

// ── Cypher fragments built from the vocabulary ───────────────────────────────

fn quoted_list(values: &[&str]) -> String {
    values
        .iter()
        .map(|v| format!("'{v}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// WHERE fragment (no leading AND) that keeps only non-terminal nodes,
/// honoring every status property plus the validation axis. This is THE
/// liveness predicate — the same set `is_retired` enforces on the vector
/// read path.
pub fn liveness_predicate(var: &str) -> String {
    let terminal = quoted_list(TERMINAL_STATUSES);
    let mut clauses = vec![format!(
        "coalesce({var}.validation_state, 'inferred') <> 'retired'"
    )];
    for prop in STATUS_PROPERTIES {
        clauses.push(format!("NOT coalesce({var}.{prop}, '') IN [{terminal}]"));
    }
    clauses.join(" AND ")
}

/// Coalesce expression selecting a node's best structured date, or null.
pub fn best_date_expr(var: &str) -> String {
    let fields = DATE_PROPERTIES
        .iter()
        .map(|p| format!("{var}.{p}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("coalesce({fields})")
}

/// The RETURN projection every deterministic list row uses. Stable field
/// set so skills and crons can rely on it.
pub fn list_row_projection(var: &str) -> String {
    format!(
        "{var}.id AS id, labels({var})[0] AS label, \
         coalesce({var}.status, {var}.loop_status) AS status, \
         coalesce({var}.validation_state, 'inferred') AS validation_state, \
         substring(coalesce({var}.claim_summary, {var}.title, ''), 0, 200) AS claim_summary, \
         {var}.observed_at AS observed_at, {best} AS best_date, \
         {var}.resolved_at AS resolved_at, {var}.retired_at AS retired_at, \
         {var}.retired_by AS retired_by",
        best = best_date_expr(var),
    )
}

// ── Named maintenance queries ────────────────────────────────────────────────

/// Deterministic, parameter-free-except-`$now` maintenance queries. These are
/// the steward's gardening primitives: each is a tested cypher template, so
/// new recipes are reviewable code additions, not prompt lore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedMaintenanceQuery {
    /// Non-terminal Events whose best structured date is in the past.
    PastDatedEvents,
    /// Non-terminal OpenLoops, oldest first (age = observed_at).
    AgingLoopsOldestFirst,
    /// Same-label live node pairs whose normalized claim prefix matches —
    /// candidates for merge/retire, mirroring the hygiene sweep's
    /// exact-duplicate collapse but surfaced for review instead of mutated.
    DuplicateCandidates,
    /// Terminal nodes, most recently observed first. NOTE: no retired_at
    /// stamp exists yet (ontology gap G2) — observed_at is a proxy.
    RecentlyRetired,
    /// Non-terminal Commitments/NextActions with a due date in the past.
    PastDueCommitments,
}

impl NamedMaintenanceQuery {
    pub const ALL: &'static [NamedMaintenanceQuery] = &[
        Self::PastDatedEvents,
        Self::AgingLoopsOldestFirst,
        Self::DuplicateCandidates,
        Self::RecentlyRetired,
        Self::PastDueCommitments,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "past_dated_events" => Some(Self::PastDatedEvents),
            "aging_loops_oldest_first" => Some(Self::AgingLoopsOldestFirst),
            "duplicate_candidates" => Some(Self::DuplicateCandidates),
            "recently_retired" => Some(Self::RecentlyRetired),
            "past_due_commitments" => Some(Self::PastDueCommitments),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PastDatedEvents => "past_dated_events",
            Self::AgingLoopsOldestFirst => "aging_loops_oldest_first",
            Self::DuplicateCandidates => "duplicate_candidates",
            Self::RecentlyRetired => "recently_retired",
            Self::PastDueCommitments => "past_due_commitments",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::PastDatedEvents => {
                "Live Events whose structured date has passed — retirement candidates. \
                 Nodes with only prose dates will NOT appear (ontology gap G1)."
            }
            Self::AgingLoopsOldestFirst => {
                "Live OpenLoops ordered oldest-first by observed_at — attention candidates."
            }
            Self::DuplicateCandidates => {
                "Live same-label node pairs with matching normalized claim prefixes — \
                 merge/retire candidates for operator review."
            }
            Self::RecentlyRetired => {
                "Terminal (retired/resolved/done) nodes, newest observed first — \
                 the steward's self-audit window."
            }
            Self::PastDueCommitments => "Live Commitments and NextActions with due_at in the past.",
        }
    }

    /// The cypher template. Binds `$now` (ISO 8601 UTC string) where dates
    /// are compared; `limit` is validated by the caller and interpolated.
    pub fn cypher(&self, limit: usize) -> String {
        match self {
            Self::PastDatedEvents => format!(
                "MATCH (n:Event) WHERE {live} AND {best} IS NOT NULL AND {best} < $now \
                 RETURN {row} ORDER BY {best} ASC LIMIT {limit}",
                live = liveness_predicate("n"),
                best = best_date_expr("n"),
                row = list_row_projection("n"),
            ),
            Self::AgingLoopsOldestFirst => format!(
                "MATCH (n:OpenLoop) WHERE {live} \
                 RETURN {row} ORDER BY coalesce(n.observed_at, n.created_at, '') ASC LIMIT {limit}",
                live = liveness_predicate("n"),
                row = list_row_projection("n"),
            ),
            Self::DuplicateCandidates => format!(
                // Cross-label on purpose: the same lived fact routinely lands
                // as e.g. an Event AND an OpenLoop (live example 2026-08-22:
                // parth_catchup_postpone Event vs parth_catchup_week OpenLoop),
                // so restricting to same-label pairs hid real duplicates.
                "MATCH (a), (b) \
                 WHERE a.id < b.id \
                 AND {live_a} AND {live_b} \
                 AND a.claim_summary IS NOT NULL AND b.claim_summary IS NOT NULL \
                 AND toLower(substring(a.claim_summary, 0, 60)) = toLower(substring(b.claim_summary, 0, 60)) \
                 RETURN a.id AS id, b.id AS duplicate_of, labels(a)[0] AS label, \
                 labels(b)[0] AS duplicate_label, \
                 substring(a.claim_summary, 0, 120) AS claim_summary \
                 LIMIT {limit}",
                live_a = liveness_predicate("a"),
                live_b = liveness_predicate("b"),
            ),
            Self::RecentlyRetired => format!(
                "MATCH (n) WHERE any(label IN labels(n) WHERE label IN [{labels}]) \
                 AND NOT ({live}) \
                 RETURN {row} \
                 ORDER BY coalesce(n.retired_at, n.resolved_at, n.observed_at, n.created_at, '') \
                 DESC LIMIT {limit}",
                labels = quoted_list(NODE_LABELS),
                live = liveness_predicate("n"),
                row = list_row_projection("n"),
            ),
            Self::PastDueCommitments => format!(
                "MATCH (n) WHERE any(label IN labels(n) WHERE label IN ['Commitment', 'NextAction']) \
                 AND {live} AND n.due_at IS NOT NULL AND n.due_at < $now \
                 RETURN {row} ORDER BY n.due_at ASC LIMIT {limit}",
                live = liveness_predicate("n"),
                row = list_row_projection("n"),
            ),
        }
    }

    fn catalog_entry(&self) -> Value {
        json!({
            "name": self.as_str(),
            "description": self.description(),
        })
    }
}

// ── Agent-facing ontology document (life.ontology) ───────────────────────────

/// The full vocabulary document served by `life.ontology`. Skills reference
/// this instead of restating vocabulary in prompt text.
pub fn ontology_document() -> Value {
    json!({
        "version": ONTOLOGY_VERSION,
        "labels": NODE_LABELS,
        "lifecycle": {
            "terminal_statuses": TERMINAL_STATUSES,
            "status_properties": {
                "canonical": "status",
                "read_also": STATUS_PROPERTIES,
                "rule": "Readers honor every status property; writers write ONLY `status`.",
            },
            "validation_states": VALIDATION_STATES,
        },
        "dates": {
            "properties": DATE_PROPERTIES,
            "format": "ISO 8601 UTC strings",
            "rule": "Writers should extract concrete dates from claims into structured \
                     fields at observe time; deterministic queries only see structured dates.",
        },
        "provenance": {
            "fields": ["observed_at", "observed_by", "observed_role", "source_membrane",
                        "provenance", "confidence", "resolved_at", "retired_by"],
            "auto_retirement_tag": "life-hygiene",
        },
        "relationships": {
            "living_cycle": crate::cypher::LIVING_CYCLE_REL_TYPES,
            "endpoint_validated": crate::cypher::AGENDA_EDGE_RULES
                .iter()
                .map(|rule| json!({
                    "rel_type": rule.rel_type,
                    "source_labels": rule.source_labels,
                    "target_labels": rule.target_labels,
                }))
                .collect::<Vec<_>>(),
            "rule": "Edges are written on life.observe. The vocabulary is CLOSED: \
                     living-cycle types are freeform-endpoint, endpoint-validated types \
                     reject wrong source labels and report target_missing for wrong \
                     targets. Prefer an edge over restating a noun in claim prose.",
        },
        "noun_guidance": {
            "Place": "Somewhere life happens (home, an office, a park, an airport). Target of OCCURS_AT.",
            "Trip": "A bounded journey with a date range (starts_at/ends_at); its Events/Appointments attach via PART_OF.",
            "Appointment": "A scheduled slot with a provider or person (medical, service). Carries due_at/starts_at; INVOLVES the people.",
            "Subscription": "A recurring paid membership or pass. Renewal work points at it via RENEWS.",
            "Asset": "A durable owned thing (vehicle, home, passport, instrument). Upkeep points at it via MAINTAINS.",
            "CreativeWork": "A piece the operator makes or maintains (music repertoire piece, writing). MAINTAINS keeps it alive.",
            "Moment": "KEPT lived history — a past experience worth remembering (a dinner, a show, a milestone). \
                       Confirmed past Events worth keeping become Moments (same INVOLVES/OCCURS_AT edges); \
                       gardening retires stale proposed Events but NEVER retires Moments.",
        },
        "named_queries": NamedMaintenanceQuery::ALL
            .iter()
            .map(NamedMaintenanceQuery::catalog_entry)
            .collect::<Vec<_>>(),
        "known_gaps": [
            {"id": "G1", "gap": "Most live nodes carry dates only in claim_summary prose; \
              structured date backfill is pending, so date-window queries under-report."},
            {"id": "G2", "gap": "retired_at is stamped by the hygiene sweep going forward, \
              but nodes retired earlier lack it; recently_retired falls back to \
              resolved_at/observed_at for those."},
            {"id": "G3", "gap": "Legacy nodes exist with numeric ids and with \
              life:open-loop (dash) vs life:open_loop (underscore) naming."},
        ],
        "rules": [
            "Terminal nodes never resurface in recall, briefs, or default lists.",
            "Every mutation must be verified by re-reading the node before it is reported.",
            "Report only node ids returned by tools; never invent ids.",
            "Prefer nouns + edges over prose: a loop ABOUT an Asset beats restating the asset in text.",
            "Kept history lives as Moment nodes; gardening never retires a Moment.",
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_statuses_include_resolved_regression_pr434() {
        assert!(is_terminal_status("resolved"));
        assert!(is_terminal_status("retired"));
        assert!(!is_terminal_status("open"));
        assert!(!is_terminal_status(""));
    }

    #[test]
    fn liveness_predicate_covers_every_status_property_and_validation() {
        let p = liveness_predicate("n");
        assert!(p.contains("n.validation_state"));
        for prop in STATUS_PROPERTIES {
            assert!(p.contains(&format!("n.{prop}")), "missing {prop} in {p}");
        }
        for status in TERMINAL_STATUSES {
            assert!(p.contains(&format!("'{status}'")), "missing {status}");
        }
    }

    #[test]
    fn named_queries_parse_round_trip() {
        for q in NamedMaintenanceQuery::ALL {
            assert_eq!(NamedMaintenanceQuery::parse(q.as_str()), Some(*q));
        }
        assert_eq!(NamedMaintenanceQuery::parse("nope"), None);
    }

    #[test]
    fn past_dated_events_binds_now_and_filters_liveness() {
        let c = NamedMaintenanceQuery::PastDatedEvents.cypher(25);
        assert!(c.contains("$now"));
        assert!(c.contains("MATCH (n:Event)"));
        assert!(c.contains("'resolved'"));
        assert!(c.contains("LIMIT 25"));
    }

    #[test]
    fn recently_retired_is_the_negation_of_liveness() {
        let c = NamedMaintenanceQuery::RecentlyRetired.cypher(10);
        assert!(c.contains("NOT ("));
        assert!(c.contains("retired_by"));
    }

    /// Lockstep: every endpoint-validated edge names only labels the write
    /// path accepts and the ontology documents; every vector-swept label is
    /// a known + documented label. Catches noun/verb drift across cypher.rs,
    /// projection.rs, and ontology.rs at compile-test time.
    #[test]
    fn vocabulary_lockstep_across_edges_spaces_and_labels() {
        for rule in crate::cypher::AGENDA_EDGE_RULES {
            for label in rule.source_labels.iter().chain(rule.target_labels) {
                assert!(
                    crate::cypher::is_known_label(label),
                    "{} endpoint {label} must be a known write label",
                    rule.rel_type
                );
                assert!(
                    is_known_label(label),
                    "{} endpoint {label} must be in the ontology",
                    rule.rel_type
                );
            }
        }
        for space in [
            crate::SemanticSpace::LifeEventSemantic,
            crate::SemanticSpace::GoalSystemSemantic,
            crate::SemanticSpace::SkillToolSemantic,
            crate::SemanticSpace::RolePersonSemantic,
            crate::SemanticSpace::MemoryBridgeSemantic,
        ] {
            for label in crate::projection::labels_for_space(&space) {
                assert!(
                    crate::projection::embedding_space_for_label(label).is_some(),
                    "swept label {label} must map to an embedding space"
                );
            }
        }
        // Every new lived-world noun is swept, so it MUST have a space (and
        // therefore a V006 index).
        for label in [
            "Place",
            "Trip",
            "Appointment",
            "Subscription",
            "Asset",
            "CreativeWork",
            "Moment",
        ] {
            assert!(
                crate::projection::embedding_space_for_label(label).is_some(),
                "lived-world noun {label} needs an embedding space + vector index"
            );
        }
    }

    #[test]
    fn ontology_document_carries_relationship_vocabulary() {
        let doc = ontology_document();
        let rels = &doc["relationships"]["endpoint_validated"];
        let names: Vec<&str> = rels
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["rel_type"].as_str().unwrap())
            .collect();
        for verb in [
            "INVOLVES",
            "OCCURS_AT",
            "PART_OF",
            "ABOUT",
            "MAINTAINS",
            "RENEWS",
        ] {
            assert!(names.contains(&verb), "missing verb {verb}");
        }
        assert!(
            doc["noun_guidance"]["Moment"]
                .as_str()
                .unwrap()
                .contains("NEVER retires")
        );
    }

    #[test]
    fn ontology_document_is_versioned_and_lists_queries() {
        let doc = ontology_document();
        assert_eq!(doc["version"], ONTOLOGY_VERSION);
        assert_eq!(
            doc["named_queries"].as_array().unwrap().len(),
            NamedMaintenanceQuery::ALL.len()
        );
        assert!(doc["known_gaps"].as_array().unwrap().len() >= 3);
    }
}
