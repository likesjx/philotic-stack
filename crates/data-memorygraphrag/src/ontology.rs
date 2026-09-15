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

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The five semantic-space index prefixes (see `projection::index_name`).
pub const SEMANTIC_SPACE_PREFIXES: &[&str] = &[
    "life_event_semantic",
    "goal_system_semantic",
    "skill_tool_semantic",
    "role_person_semantic",
    "memory_bridge_semantic",
];

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

// ── Runtime ontology extensions (governed self-serve vocabulary) ─────────────
//
// The compiled vocabulary above is the CORE. Extensions are new labels and
// endpoint-validated edges added at RUNTIME through the governed patch
// pipeline: the steward proposes them via `life.patch.propose`
// (`patch_kind: schema_patch`, `ontology_extension` payload), the operator
// confirms, and `life.patch.apply` validates the spec, creates the vector
// index for each new label, and persists the merged set on the
// `OntologyExtension` graph node. Every read/write surface consults
// core ∪ extensions, so an applied extension is live vocabulary immediately —
// no code change, no deploy. Structural changes (new named queries, date
// semantics, new spaces) still go through code.

/// A label added at runtime. `space` must be one of
/// [`SEMANTIC_SPACE_PREFIXES`]; apply creates `{space}__{name}` in Memgraph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionLabel {
    pub name: String,
    pub space: String,
    #[serde(default)]
    pub guidance: String,
}

/// An endpoint-validated relationship added at runtime. Same closed-contract
/// semantics as [`crate::cypher::AGENDA_EDGE_RULES`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionEdge {
    pub rel_type: String,
    pub source_labels: Vec<String>,
    pub target_labels: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OntologyExtensions {
    #[serde(default)]
    pub labels: Vec<ExtensionLabel>,
    #[serde(default)]
    pub edges: Vec<ExtensionEdge>,
}

/// Identifier shape for an extension LABEL: PascalCase, letters/digits only.
/// Interpolated into Cypher (index names, label predicates) — the shape gate
/// IS the injection guard.
pub fn valid_extension_label_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
        && name.len() >= 2
        && name.len() <= 40
        && chars.all(|c| c.is_ascii_alphanumeric())
}

/// Identifier shape for an extension REL_TYPE: SCREAMING_SNAKE.
pub fn valid_extension_rel_type(rel: &str) -> bool {
    let mut chars = rel.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
        && rel.len() >= 2
        && rel.len() <= 40
        && chars.all(|c| c.is_ascii_uppercase() || c == '_')
}

impl OntologyExtensions {
    pub fn is_extension_label(&self, label: &str) -> bool {
        self.labels.iter().any(|l| l.name == label)
    }

    pub fn edge(&self, rel_type: &str) -> Option<&ExtensionEdge> {
        self.edges.iter().find(|e| e.rel_type == rel_type)
    }

    /// Extension labels swept by a given semantic-space index prefix.
    pub fn labels_for_space_prefix(&self, prefix: &str) -> Vec<&str> {
        self.labels
            .iter()
            .filter(|l| l.space == prefix)
            .map(|l| l.name.as_str())
            .collect()
    }

    /// Validate a spec against the core vocabulary plus `existing` extensions
    /// (endpoints may reference either). Collisions with core names are
    /// rejected — an extension can never shadow compiled vocabulary.
    pub fn validate_against(&self, existing: &OntologyExtensions) -> Result<(), Vec<String>> {
        let mut violations = Vec::new();
        if self.labels.is_empty() && self.edges.is_empty() {
            violations.push("ontology_extension must add at least one label or edge".into());
        }
        for label in &self.labels {
            if !valid_extension_label_name(&label.name) {
                violations.push(format!(
                    "label '{}' is not a valid identifier (PascalCase, 2-40 alphanumeric chars)",
                    label.name
                ));
            }
            if crate::cypher::is_known_label(&label.name) {
                violations.push(format!(
                    "label '{}' collides with the compiled core vocabulary",
                    label.name
                ));
            }
            if !SEMANTIC_SPACE_PREFIXES.contains(&label.space.as_str()) {
                violations.push(format!(
                    "label '{}' names unknown space '{}' (expected one of: {})",
                    label.name,
                    label.space,
                    SEMANTIC_SPACE_PREFIXES.join(", ")
                ));
            }
        }
        let label_known = |name: &str| {
            crate::cypher::is_known_label(name)
                || existing.is_extension_label(name)
                || self.labels.iter().any(|l| l.name == name)
        };
        for edge in &self.edges {
            if !valid_extension_rel_type(&edge.rel_type) {
                violations.push(format!(
                    "rel_type '{}' is not a valid identifier (SCREAMING_SNAKE, 2-40 chars)",
                    edge.rel_type
                ));
            }
            if crate::cypher::is_living_cycle_rel_type(&edge.rel_type)
                || crate::cypher::is_agenda_rel_type(&edge.rel_type)
            {
                violations.push(format!(
                    "rel_type '{}' collides with the compiled core vocabulary",
                    edge.rel_type
                ));
            }
            if edge.source_labels.is_empty() || edge.target_labels.is_empty() {
                violations.push(format!(
                    "rel_type '{}' must declare source_labels and target_labels",
                    edge.rel_type
                ));
            }
            for endpoint in edge.source_labels.iter().chain(&edge.target_labels) {
                if !label_known(endpoint) {
                    violations.push(format!(
                        "rel_type '{}' endpoint '{}' is not a known or extension label",
                        edge.rel_type, endpoint
                    ));
                }
            }
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(violations)
        }
    }

    /// Merge `incoming` into self: same-name labels / same-rel edges are
    /// REPLACED (the newly applied spec wins), everything else appends.
    pub fn merge(&mut self, incoming: OntologyExtensions) {
        for label in incoming.labels {
            self.labels.retain(|l| l.name != label.name);
            self.labels.push(label);
        }
        for edge in incoming.edges {
            self.edges.retain(|e| e.rel_type != edge.rel_type);
            self.edges.push(edge);
        }
    }
}

/// The agent-facing vocabulary document with runtime extensions merged in.
pub fn ontology_document_with(ext: &OntologyExtensions) -> Value {
    let mut doc = ontology_document();
    if let Some(labels) = doc["labels"].as_array_mut() {
        for label in &ext.labels {
            labels.push(json!(label.name));
        }
    }
    doc["extensions"] = json!({
        "labels": ext.labels,
        "edges": ext.edges,
        "rule": "Runtime extensions added through the governed patch pipeline \
                 (life.patch.propose patch_kind=schema_patch with an \
                 ontology_extension payload → operator confirm → \
                 life.patch.apply). They are full vocabulary: writable, swept, \
                 listable.",
    });
    if let Some(guidance) = doc["noun_guidance"].as_object_mut() {
        for label in &ext.labels {
            if !label.guidance.is_empty() {
                guidance.insert(label.name.clone(), json!(label.guidance));
            }
        }
    }
    doc
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

    /// Extension-aware variant: queries that enumerate the label universe
    /// include runtime extension labels too.
    pub fn cypher_with_extensions(&self, limit: usize, ext: &OntologyExtensions) -> String {
        match self {
            Self::RecentlyRetired if !ext.labels.is_empty() => {
                let mut labels: Vec<&str> = NODE_LABELS.to_vec();
                labels.extend(ext.labels.iter().map(|l| l.name.as_str()));
                format!(
                    "MATCH (n) WHERE any(label IN labels(n) WHERE label IN [{labels}]) \
                     AND NOT ({live}) \
                     RETURN {row} \
                     ORDER BY coalesce(n.retired_at, n.resolved_at, n.observed_at, n.created_at, '') \
                     DESC LIMIT {limit}",
                    labels = quoted_list(&labels),
                    live = liveness_predicate("n"),
                    row = list_row_projection("n"),
                )
            }
            _ => self.cypher(limit),
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

    fn sample_extension() -> OntologyExtensions {
        OntologyExtensions {
            labels: vec![ExtensionLabel {
                name: "Pet".into(),
                space: "life_event_semantic".into(),
                guidance: "A companion animal.".into(),
            }],
            edges: vec![ExtensionEdge {
                rel_type: "CARES_FOR".into(),
                source_labels: vec!["Routine".into(), "Person".into()],
                target_labels: vec!["Pet".into()],
            }],
        }
    }

    #[test]
    fn extension_validation_accepts_well_formed_and_rejects_bad_specs() {
        let ext = sample_extension();
        assert!(ext.validate_against(&OntologyExtensions::default()).is_ok());

        // Core collision, bad identifier, unknown space, unknown endpoint,
        // core rel collision — every class of violation reported.
        let bad = OntologyExtensions {
            labels: vec![
                ExtensionLabel {
                    name: "Event".into(),
                    space: "life_event_semantic".into(),
                    guidance: String::new(),
                },
                ExtensionLabel {
                    name: "drop table".into(),
                    space: "nope_space".into(),
                    guidance: String::new(),
                },
            ],
            edges: vec![ExtensionEdge {
                rel_type: "ABOUT".into(),
                source_labels: vec!["Ghost".into()],
                target_labels: vec![],
            }],
        };
        let violations = bad
            .validate_against(&OntologyExtensions::default())
            .expect_err("bad spec must fail");
        assert!(violations.iter().any(|v| v.contains("collides")));
        assert!(
            violations
                .iter()
                .any(|v| v.contains("not a valid identifier"))
        );
        assert!(violations.iter().any(|v| v.contains("unknown space")));
        assert!(violations.iter().any(|v| v.contains("Ghost")));
    }

    #[test]
    fn extension_merge_replaces_same_name_entries() {
        let mut current = sample_extension();
        current.merge(OntologyExtensions {
            labels: vec![ExtensionLabel {
                name: "Pet".into(),
                space: "life_event_semantic".into(),
                guidance: "Updated guidance.".into(),
            }],
            edges: vec![],
        });
        assert_eq!(current.labels.len(), 1);
        assert_eq!(current.labels[0].guidance, "Updated guidance.");
        assert_eq!(current.edges.len(), 1);
    }

    #[test]
    fn extension_labels_join_document_lists_and_named_queries() {
        let ext = sample_extension();
        let doc = ontology_document_with(&ext);
        assert!(doc["labels"].as_array().unwrap().iter().any(|l| l == "Pet"));
        assert_eq!(doc["noun_guidance"]["Pet"], "A companion animal.");
        assert_eq!(doc["extensions"]["edges"][0]["rel_type"], "CARES_FOR");

        let cypher = NamedMaintenanceQuery::RecentlyRetired.cypher_with_extensions(5, &ext);
        assert!(cypher.contains("'Pet'"), "{cypher}");
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
