//! Procedural graphs — learned execution structure under grounded
//! verification (doc:procedural-graphs, slice P0 `procedure-graph-record`).
//!
//! A [`ProcedureGraphRecord`] is the Philotic form of the paper's
//! `G = (V, R, E, Φ)` (Lu, Chen, Wu, Arık et al., arXiv 2609.09153): tool-bound
//! nodes, typed edges, and per-edge `condition / guidance / pitfalls` text.
//! It lives in the hotel context graph beside `abstract_skill`, projects into
//! a session's bindings when its skill is in play, and is read by philote
//! locally — localization never needs an IPC round trip.
//!
//! Three things are deliberately *not* here: a guidance model (the render is
//! deterministic, see philote's `procedure_guidance`), free-text node
//! matching (only `tool_name` is ever matched), and any edit operation (P4
//! adds patches; a record in this slice changes only through re-registration).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::graph::SkillValidationState;

/// Hard cap on nodes per procedure. The paper's graphs run 7–17 nodes outside
/// function-calling catalogs; the cap keeps a rendered neighbourhood bounded.
pub const MAX_PROCEDURE_NODES: usize = 64;
/// Hard cap on edges per procedure.
pub const MAX_PROCEDURE_EDGES: usize = 128;
/// Per-field text cap for labels and edge attributes.
pub const MAX_PROCEDURE_TEXT_CHARS: usize = 280;
/// Description cap (one paragraph).
pub const MAX_PROCEDURE_DESCRIPTION_CHARS: usize = 600;

/// What a node stands for. Only `Tool` nodes are ever localized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProcedureNodeKind {
    #[default]
    Tool,
    Reasoning,
    State,
}

/// The paper's transition vocabulary, unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProcedureRelation {
    #[default]
    LeadsTo,
    Triggers,
    ProvidesInputFor,
    ConvergesTo,
}

impl ProcedureRelation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LeadsTo => "LEADS_TO",
            Self::Triggers => "TRIGGERS",
            Self::ProvidesInputFor => "PROVIDES_INPUT_FOR",
            Self::ConvergesTo => "CONVERGES_TO",
        }
    }

    /// Relations a backbone walk may follow: the ones that mean "then do".
    fn is_sequencing(&self) -> bool {
        matches!(self, Self::LeadsTo | Self::Triggers | Self::ConvergesTo)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProcedureNode {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub kind: ProcedureNodeKind,
    /// Required for `Tool` nodes; the exact tool name a step binds to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProcedureEdge {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub relation: ProcedureRelation,
    /// When the transition applies.
    #[serde(default)]
    pub condition: String,
    /// How to proceed.
    #[serde(default)]
    pub guidance: String,
    /// What to avoid.
    #[serde(default)]
    pub pitfalls: String,
    /// Machine-readable branch tag for an edge out of the entry node, so a
    /// seeder can choose a branch without parsing `condition` prose (e.g.
    /// `target_known` / `target_unknown`). Free text otherwise ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// Where a procedure came from. Mirrors the provenance the self-improvement
/// loop asks for on skills; `Refiner` is reserved for P4 patch-produced
/// versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProcedureProvenance {
    #[default]
    Repo,
    Operator,
    Agent {
        agent_id: String,
    },
    Refiner,
}

/// A procedural graph stored in the hotel context graph.
///
/// Node kind: `procedure`. Node key: `procedure:{procedure_id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcedureGraphRecord {
    /// Lowercase dotted / dashed id, like a skill name.
    pub procedure_id: String,
    pub description: String,
    /// The skill whose projection carries this procedure. `None` = standalone;
    /// a standalone record projects only when it has a `trigger`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_name: Option<String>,
    /// Name of a compiled-in trigger predicate in philote (e.g.
    /// `reports_an_outcome`). Predicates are code; the record only names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// Node id the backbone starts from.
    pub entry: String,
    #[serde(default)]
    pub nodes: Vec<ProcedureNode>,
    #[serde(default)]
    pub edges: Vec<ProcedureEdge>,
    #[serde(default = "default_version")]
    pub version: u32,
    /// Patch id while this version is a candidate under trial (P4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_of: Option<String>,
    #[serde(default)]
    pub provenance: ProcedureProvenance,
    #[serde(default)]
    pub validation_state: SkillValidationState,
    #[serde(default)]
    pub updated_at: u64,
}

fn default_version() -> u32 {
    1
}

impl Default for ProcedureGraphRecord {
    fn default() -> Self {
        Self {
            procedure_id: String::new(),
            description: String::new(),
            skill_name: None,
            trigger: None,
            entry: String::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
            version: 1,
            trial_of: None,
            provenance: ProcedureProvenance::Repo,
            validation_state: SkillValidationState::Draft,
            updated_at: 0,
        }
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_'))
}

impl ProcedureGraphRecord {
    /// Mechanical validation, run on register and on every patch. Returns
    /// every problem found rather than the first, so an author can fix a
    /// record in one pass. Prompt-safety scanning is the caller's job
    /// (`text_fields` hands over every string that will reach a prompt).
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        if !valid_id(&self.procedure_id) {
            errors.push(format!(
                "procedure_id {:?} must be non-empty lowercase [a-z0-9._-] (≤96 chars)",
                self.procedure_id
            ));
        }
        if self.description.trim().is_empty() {
            errors.push("description must not be empty".into());
        }
        if self.description.chars().count() > MAX_PROCEDURE_DESCRIPTION_CHARS {
            errors.push(format!(
                "description exceeds {MAX_PROCEDURE_DESCRIPTION_CHARS} chars"
            ));
        }
        if self.nodes.is_empty() {
            errors.push("a procedure needs at least one node".into());
        }
        if self.nodes.len() > MAX_PROCEDURE_NODES {
            errors.push(format!(
                "{} nodes exceeds the cap of {MAX_PROCEDURE_NODES}",
                self.nodes.len()
            ));
        }
        if self.edges.len() > MAX_PROCEDURE_EDGES {
            errors.push(format!(
                "{} edges exceeds the cap of {MAX_PROCEDURE_EDGES}",
                self.edges.len()
            ));
        }
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for node in &self.nodes {
            if !valid_id(&node.id) {
                errors.push(format!(
                    "node id {:?} must be non-empty lowercase [a-z0-9._-]",
                    node.id
                ));
            }
            if !seen.insert(node.id.as_str()) {
                errors.push(format!("duplicate node id {:?}", node.id));
            }
            if node.label.trim().is_empty() {
                errors.push(format!("node {:?} has an empty label", node.id));
            }
            if node.label.chars().count() > MAX_PROCEDURE_TEXT_CHARS {
                errors.push(format!(
                    "node {:?} label exceeds {MAX_PROCEDURE_TEXT_CHARS} chars",
                    node.id
                ));
            }
            match (node.kind, node.tool_name.as_deref()) {
                (ProcedureNodeKind::Tool, None | Some("")) => {
                    errors.push(format!("tool node {:?} has no tool_name", node.id));
                }
                (ProcedureNodeKind::Tool, Some(t))
                    if t.trim() != t || t.contains(char::is_whitespace) =>
                {
                    errors.push(format!(
                        "tool node {:?} tool_name {t:?} must not contain whitespace",
                        node.id
                    ));
                }
                _ => {}
            }
        }
        if !seen.contains(self.entry.as_str()) {
            errors.push(format!("entry {:?} is not a node id", self.entry));
        }
        for (i, edge) in self.edges.iter().enumerate() {
            if !seen.contains(edge.from.as_str()) {
                errors.push(format!(
                    "edge {i} from {:?} references a node that does not exist",
                    edge.from
                ));
            }
            if !seen.contains(edge.to.as_str()) {
                errors.push(format!(
                    "edge {i} to {:?} references a node that does not exist",
                    edge.to
                ));
            }
            if edge.from == edge.to {
                errors.push(format!("edge {i} is a self-loop on {:?}", edge.from));
            }
            for (field, text) in [
                ("condition", &edge.condition),
                ("guidance", &edge.guidance),
                ("pitfalls", &edge.pitfalls),
            ] {
                if text.chars().count() > MAX_PROCEDURE_TEXT_CHARS {
                    errors.push(format!(
                        "edge {i} {field} exceeds {MAX_PROCEDURE_TEXT_CHARS} chars"
                    ));
                }
            }
        }
        let mut edge_keys: BTreeSet<(&str, &str, &str)> = BTreeSet::new();
        for edge in &self.edges {
            if !edge_keys.insert((&edge.from, &edge.to, edge.relation.as_str())) {
                errors.push(format!(
                    "duplicate edge {} -{}-> {}",
                    edge.from,
                    edge.relation.as_str(),
                    edge.to
                ));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Every string that will be rendered into a prompt, for the prompt-guard
    /// scan at the registration boundary.
    pub fn text_fields(&self) -> Vec<&str> {
        let mut out = vec![self.description.as_str()];
        for node in &self.nodes {
            out.push(node.label.as_str());
        }
        for edge in &self.edges {
            out.push(edge.condition.as_str());
            out.push(edge.guidance.as_str());
            out.push(edge.pitfalls.as_str());
        }
        out
    }

    pub fn node(&self, id: &str) -> Option<&ProcedureNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Outgoing edges of `id`, in declaration order.
    pub fn outgoing(&self, id: &str) -> Vec<&ProcedureEdge> {
        self.edges.iter().filter(|e| e.from == id).collect()
    }

    /// Incoming edges of `id`, in declaration order.
    pub fn incoming(&self, id: &str) -> Vec<&ProcedureEdge> {
        self.edges.iter().filter(|e| e.to == id).collect()
    }

    /// Distinct tool names bound by this procedure's tool nodes.
    pub fn tool_names(&self) -> BTreeSet<&str> {
        self.nodes
            .iter()
            .filter(|n| n.kind == ProcedureNodeKind::Tool)
            .filter_map(|n| n.tool_name.as_deref())
            .collect()
    }

    /// Jaccard overlap between this procedure's tools and a plan's declared
    /// tools. Used to attribute an un-stamped plan to a procedure.
    pub fn tool_overlap(&self, tools: &[String]) -> f32 {
        let mine = self.tool_names();
        let theirs: BTreeSet<&str> = tools.iter().map(String::as_str).collect();
        if mine.is_empty() && theirs.is_empty() {
            return 0.0;
        }
        let inter = mine.intersection(&theirs).count() as f32;
        let union = mine.union(&theirs).count() as f32;
        inter / union
    }

    /// The linear backbone from `entry`: follow the first sequencing edge
    /// (`LEADS_TO` / `TRIGGERS` / `CONVERGES_TO`, declaration order) out of
    /// each node with a visited set, so a cycle terminates. This is the only
    /// projection a plan is ever seeded from.
    pub fn linear_backbone(&self) -> Vec<&ProcedureNode> {
        self.backbone_from(&self.entry)
    }

    /// Backbone starting at an arbitrary node — the seeder uses this to enter a
    /// branch the trigger context has already chosen.
    pub fn backbone_from(&self, start: &str) -> Vec<&ProcedureNode> {
        let mut out = Vec::new();
        let mut visited: BTreeSet<&str> = BTreeSet::new();
        let mut cursor = self.node(start);
        while let Some(node) = cursor {
            if !visited.insert(node.id.as_str()) {
                break;
            }
            out.push(node);
            cursor = self
                .outgoing(&node.id)
                .into_iter()
                .find(|e| e.relation.is_sequencing())
                .and_then(|e| self.node(&e.to));
        }
        out
    }

    /// Localize the agent: the tool node matching the last successful tool
    /// call. Exact match on `tool_name`, like the paper. When several nodes
    /// share a tool, the one whose predecessor matches the previous call wins;
    /// otherwise the first in declaration order.
    pub fn locate(&self, last_tool: &str, previous_tool: Option<&str>) -> Option<&ProcedureNode> {
        let candidates: Vec<&ProcedureNode> = self
            .nodes
            .iter()
            .filter(|n| {
                n.kind == ProcedureNodeKind::Tool && n.tool_name.as_deref() == Some(last_tool)
            })
            .collect();
        match candidates.as_slice() {
            [] => None,
            [only] => Some(only),
            many => {
                if let Some(prev) = previous_tool {
                    for candidate in many {
                        let predecessor_matches = self.incoming(&candidate.id).iter().any(|e| {
                            self.node(&e.from)
                                .and_then(|n| n.tool_name.as_deref())
                                .is_some_and(|t| t == prev)
                        });
                        if predecessor_matches {
                            return Some(candidate);
                        }
                    }
                }
                many.first().copied()
            }
        }
    }

    /// Serialize as the paper's triplet list, for the refiner prompt and
    /// `phil procedure show`.
    pub fn render_triplets(&self) -> String {
        let mut out = String::new();
        for edge in &self.edges {
            out.push_str(&format!(
                "({}, {}, {}) condition: {} | guidance: {} | pitfalls: {}\n",
                edge.from,
                edge.relation.as_str(),
                edge.to,
                edge.condition,
                edge.guidance,
                edge.pitfalls
            ));
        }
        out
    }
}

/// One terminal plan evaluation attributed to a procedure.
///
/// Node kind: `procedure_run`. Node key: `procedure_run:{run_id}`. Append-only.
/// Written only when a plan reaches a terminal verdict (Complete, Blocked, or a
/// budget stop) — never on `Continue` — and only when a procedure matched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProcedureRunRecord {
    pub run_id: String,
    pub procedure_id: String,
    #[serde(default = "default_version")]
    pub graph_version: u32,
    pub agent_id: String,
    pub session_id: String,
    pub turn_id: String,
    /// Plan goal, truncated at the sender.
    #[serde(default)]
    pub goal: String,
    /// Tool names in the order they were called across the plan's turns
    /// (the working history of the terminal turn).
    #[serde(default)]
    pub tool_sequence: Vec<String>,
    /// `complete` | `blocked` | `stopped`.
    pub verdict: String,
    /// `grounded` | `model_reported`.
    #[serde(default)]
    pub basis: String,
    #[serde(default)]
    pub steps_total: usize,
    #[serde(default)]
    pub steps_verified: usize,
    #[serde(default)]
    pub steps_done: usize,
    #[serde(default)]
    pub stalls: u32,
    #[serde(default)]
    pub non_atomic: usize,
    #[serde(default)]
    pub contradicted: usize,
    /// Whether procedure guidance was rendered into any turn of this plan.
    #[serde(default)]
    pub guidance_rendered: bool,
    /// The validation score: see [`ProcedureRunRecord::score_for`].
    #[serde(default)]
    pub score: f32,
    #[serde(default)]
    pub recorded_at: u64,
}

impl ProcedureRunRecord {
    /// The paper's per-episode score `S`, reduced to what grounded evaluation
    /// can vouch for: 1.0 for a complete plan verified against tool results,
    /// 0.5 for a plan the model alone declared complete, 0.0 otherwise.
    pub fn score_for(verdict: &str, basis: &str) -> f32 {
        match (verdict, basis) {
            ("complete", "grounded") => 1.0,
            ("complete", _) => 0.5,
            _ => 0.0,
        }
    }

    pub fn is_success(&self) -> bool {
        self.score >= 1.0
    }
}

/// The expert-prior seed for the outcome reflex — the graph form of the two
/// literal plan shapes `SessionState::seed_outcome_plan` carried before P3.
///
/// ```text
/// start ─TRIGGERS(no loop in context)──▶ recall ─LEADS_TO──▶ observe ─LEADS_TO──▶ commit
///   └───TRIGGERS(loop already recalled)──────────────────────▲
/// ```
pub fn outcome_reflex_procedure() -> ProcedureGraphRecord {
    ProcedureGraphRecord {
        procedure_id: "outcome-reflex".into(),
        description: "When the operator reports that something happened (\"I gave my speech\", \
                      \"tickets are booked\"), record it as a confirmed Event and resolve the \
                      LifeGraph loop it settles. A reported outcome is a write, never just a reply."
            .into(),
        skill_name: Some("life.steward".into()),
        trigger: Some("reports_an_outcome".into()),
        entry: "start".into(),
        nodes: vec![
            ProcedureNode {
                id: "start".into(),
                label: "Operator reported an outcome".into(),
                kind: ProcedureNodeKind::State,
                tool_name: None,
            },
            ProcedureNode {
                id: "recall".into(),
                label: "Find the open loop, commitment, or next action this outcome settles".into(),
                kind: ProcedureNodeKind::Tool,
                tool_name: Some("life.recall".into()),
            },
            ProcedureNode {
                id: "observe".into(),
                label: "Record the reported outcome as a confirmed Event — what happened, when, who was involved".into(),
                kind: ProcedureNodeKind::Tool,
                tool_name: Some("life.observe".into()),
            },
            ProcedureNode {
                id: "commit".into(),
                label: "Resolve the loop by its exact id".into(),
                kind: ProcedureNodeKind::Tool,
                tool_name: Some("life.commit".into()),
            },
        ],
        edges: vec![
            ProcedureEdge {
                from: "start".into(),
                to: "recall".into(),
                relation: ProcedureRelation::Triggers,
                condition: "no recalled loop for this outcome is in context".into(),
                guidance: "life.recall with named_strategy \"open_loops_by_context\" and query_text = the operator's message".into(),
                pitfalls: "skipping straight to observe with the loop unknown; guessing an id".into(),
                branch: Some("target_unknown".into()),
            },
            ProcedureEdge {
                from: "start".into(),
                to: "observe".into(),
                relation: ProcedureRelation::Triggers,
                condition: "the loop this outcome settles is already recalled in context".into(),
                guidance: "record the Event linked to {target}".into(),
                pitfalls: "congratulating and stopping — a reported outcome is a write, not a reply".into(),
                branch: Some("target_known".into()),
            },
            ProcedureEdge {
                from: "recall".into(),
                to: "observe".into(),
                relation: ProcedureRelation::LeadsTo,
                condition: "recall returned, even if it found nothing".into(),
                guidance: "record the Event linked to {target}, or standalone when the recall matched nothing".into(),
                pitfalls: "re-running recall with a rephrased query instead of moving on".into(),
                branch: None,
            },
            ProcedureEdge {
                from: "observe".into(),
                to: "commit".into(),
                relation: ProcedureRelation::LeadsTo,
                condition: "the outcome Event is recorded".into(),
                guidance: "life.commit {target} by its exact id: loop_status \"resolved\", resolution_note citing the Event; with no loop, commit the Event's own node id as confirmed".into(),
                pitfalls: "inventing an id; resolving a node that was not recalled; claiming the write in text without the call".into(),
                branch: None,
            },
        ],
        version: 1,
        trial_of: None,
        provenance: ProcedureProvenance::Repo,
        validation_state: SkillValidationState::Validated,
        updated_at: 0,
    }
}

/// Every repo-provenance procedure seeded at hotel boot.
pub fn seeded_procedures() -> Vec<ProcedureGraphRecord> {
    vec![outcome_reflex_procedure()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(id: &str, tool_name: &str) -> ProcedureNode {
        ProcedureNode {
            id: id.into(),
            label: format!("do {id}"),
            kind: ProcedureNodeKind::Tool,
            tool_name: Some(tool_name.into()),
        }
    }

    fn edge(from: &str, to: &str, relation: ProcedureRelation) -> ProcedureEdge {
        ProcedureEdge {
            from: from.into(),
            to: to.into(),
            relation,
            ..Default::default()
        }
    }

    fn chain() -> ProcedureGraphRecord {
        ProcedureGraphRecord {
            procedure_id: "test.chain".into(),
            description: "a b c".into(),
            entry: "a".into(),
            nodes: vec![tool("a", "t.a"), tool("b", "t.b"), tool("c", "t.c")],
            edges: vec![
                edge("a", "b", ProcedureRelation::LeadsTo),
                edge("b", "c", ProcedureRelation::LeadsTo),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn seeded_procedures_validate() {
        for p in seeded_procedures() {
            assert_eq!(p.validate(), Ok(()), "{}", p.procedure_id);
        }
    }

    #[test]
    fn validate_rejects_dangling_edge_missing_entry_and_self_loop() {
        let mut p = chain();
        p.edges.push(edge("c", "zzz", ProcedureRelation::LeadsTo));
        p.edges.push(edge("a", "a", ProcedureRelation::LeadsTo));
        p.entry = "nope".into();
        let errors = p.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("zzz")), "{errors:?}");
        assert!(errors.iter().any(|e| e.contains("self-loop")), "{errors:?}");
        assert!(errors.iter().any(|e| e.contains("entry")), "{errors:?}");
    }

    #[test]
    fn validate_rejects_oversize_and_bad_ids() {
        let mut p = chain();
        p.nodes = (0..(MAX_PROCEDURE_NODES + 1))
            .map(|i| tool(&format!("n{i}"), "t"))
            .collect();
        p.entry = "n0".into();
        let errors = p.validate().unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("exceeds the cap")),
            "{errors:?}"
        );

        let mut p = chain();
        p.procedure_id = "Bad Id".into();
        p.nodes[0].tool_name = None;
        p.edges[0].guidance = "x".repeat(MAX_PROCEDURE_TEXT_CHARS + 1);
        let errors = p.validate().unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("procedure_id")),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("no tool_name")),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("guidance exceeds")),
            "{errors:?}"
        );
    }

    #[test]
    fn validate_rejects_duplicate_nodes_and_edges() {
        let mut p = chain();
        p.nodes.push(tool("a", "t.a2"));
        p.edges.push(edge("a", "b", ProcedureRelation::LeadsTo));
        let errors = p.validate().unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("duplicate node")),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("duplicate edge")),
            "{errors:?}"
        );
    }

    #[test]
    fn backbone_follows_first_sequencing_edge_and_terminates_on_cycles() {
        let mut p = chain();
        // A non-sequencing edge out of `a` first: must be skipped.
        p.edges
            .insert(0, edge("a", "c", ProcedureRelation::ProvidesInputFor));
        // A cycle back to the start: must terminate.
        p.edges.push(edge("c", "a", ProcedureRelation::LeadsTo));
        assert_eq!(p.validate(), Ok(()));
        let ids: Vec<&str> = p.linear_backbone().iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
        let ids: Vec<&str> = p.backbone_from("b").iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "c", "a"]);
        assert!(p.backbone_from("missing").is_empty());
    }

    #[test]
    fn outcome_reflex_backbone_is_recall_observe_commit_with_an_observe_branch() {
        let p = outcome_reflex_procedure();
        let ids: Vec<&str> = p.linear_backbone().iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["start", "recall", "observe", "commit"]);
        let ids: Vec<&str> = p
            .backbone_from("observe")
            .iter()
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(ids, vec!["observe", "commit"]);
        assert_eq!(
            p.tool_names().into_iter().collect::<Vec<_>>(),
            vec!["life.commit", "life.observe", "life.recall"]
        );
    }

    #[test]
    fn locate_exact_matches_and_disambiguates_by_predecessor() {
        let mut p = chain();
        // Two nodes bound to the same tool, reached from different predecessors.
        p.nodes.push(tool("c2", "t.c"));
        p.edges.push(edge("a", "c2", ProcedureRelation::LeadsTo));
        assert_eq!(p.validate(), Ok(()));
        assert_eq!(p.locate("t.b", None).map(|n| n.id.as_str()), Some("b"));
        assert_eq!(
            p.locate("t.c", Some("t.b")).map(|n| n.id.as_str()),
            Some("c")
        );
        assert_eq!(
            p.locate("t.c", Some("t.a")).map(|n| n.id.as_str()),
            Some("c2")
        );
        // Unknown predecessor: first in declaration order.
        assert_eq!(
            p.locate("t.c", Some("t.zzz")).map(|n| n.id.as_str()),
            Some("c")
        );
        assert_eq!(p.locate("t.nope", None), None);
        // State nodes are never localized.
        let o = outcome_reflex_procedure();
        assert_eq!(o.locate("start", None), None);
    }

    #[test]
    fn tool_overlap_is_jaccard() {
        let p = chain();
        assert_eq!(
            p.tool_overlap(&["t.a".into(), "t.b".into(), "t.c".into()]),
            1.0
        );
        assert!((p.tool_overlap(&["t.a".into(), "t.x".into()]) - 0.25).abs() < 1e-6);
        assert_eq!(p.tool_overlap(&[]), 0.0);
    }

    #[test]
    fn run_score_follows_grounded_verification() {
        assert_eq!(ProcedureRunRecord::score_for("complete", "grounded"), 1.0);
        assert_eq!(
            ProcedureRunRecord::score_for("complete", "model_reported"),
            0.5
        );
        assert_eq!(ProcedureRunRecord::score_for("blocked", "grounded"), 0.0);
        assert_eq!(ProcedureRunRecord::score_for("stopped", "grounded"), 0.0);
    }

    #[test]
    fn record_round_trips_with_defaults() {
        let p = outcome_reflex_procedure();
        let json = serde_json::to_value(&p).unwrap();
        let back: ProcedureGraphRecord = serde_json::from_value(json).unwrap();
        assert_eq!(back, p);
        // An older record without version/provenance still loads.
        let minimal: ProcedureGraphRecord = serde_json::from_value(serde_json::json!({
            "procedure_id": "x.y",
            "description": "d",
            "entry": "a",
            "nodes": [{"id": "a", "label": "A", "tool_name": "t"}],
        }))
        .unwrap();
        assert_eq!(minimal.version, 1);
        assert_eq!(minimal.provenance, ProcedureProvenance::Repo);
        assert_eq!(minimal.validate(), Ok(()));
    }
}
