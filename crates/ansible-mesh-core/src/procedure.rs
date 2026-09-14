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

// ── P4: patches and the trial gate ───────────────────────────────────────────

/// Hard cap on ops per patch. The paper's refiner edits are small, targeted
/// add/delete sets; a patch that rewrites half the graph is a re-registration.
pub const MAX_PATCH_OPS: usize = 16;
/// Default terminal runs a candidate version must accumulate before the trial
/// decides (`PHILOTIC_PROCEDURE_TRIAL_RUNS`).
pub const DEFAULT_TRIAL_RUNS: usize = 5;
/// Score bar for a candidate when its procedure has no baseline runs at all.
pub const TRIAL_BAR_WITHOUT_BASELINE: f32 = 0.5;

/// One edit to a procedure. The paper's refiner emits add/delete sets;
/// attribute rewrites are a delete + re-add there and a `set_*` here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ProcedurePatchOp {
    AddNode {
        node: ProcedureNode,
    },
    /// Removes the node and every edge incident to it.
    DeleteNode {
        id: String,
    },
    AddEdge {
        edge: ProcedureEdge,
    },
    DeleteEdge {
        from: String,
        to: String,
        #[serde(default)]
        relation: ProcedureRelation,
    },
    SetEdgeAttrs {
        from: String,
        to: String,
        #[serde(default)]
        relation: ProcedureRelation,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        condition: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        guidance: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pitfalls: Option<String>,
    },
    SetNodeLabel {
        id: String,
        label: String,
    },
}

impl ProcedurePatchOp {
    /// Every string an op could put into a prompt, for the prompt-guard scan.
    pub fn text_fields(&self) -> Vec<&str> {
        match self {
            Self::AddNode { node } => vec![node.label.as_str()],
            Self::AddEdge { edge } => vec![
                edge.condition.as_str(),
                edge.guidance.as_str(),
                edge.pitfalls.as_str(),
            ],
            Self::SetEdgeAttrs {
                condition,
                guidance,
                pitfalls,
                ..
            } => [condition, guidance, pitfalls]
                .into_iter()
                .flatten()
                .map(String::as_str)
                .collect(),
            Self::SetNodeLabel { label, .. } => vec![label.as_str()],
            Self::DeleteNode { .. } | Self::DeleteEdge { .. } => Vec::new(),
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::AddNode { node } => format!("add_node {}", node.id),
            Self::DeleteNode { id } => format!("delete_node {id}"),
            Self::AddEdge { edge } => {
                format!(
                    "add_edge {} -{}-> {}",
                    edge.from,
                    edge.relation.as_str(),
                    edge.to
                )
            }
            Self::DeleteEdge { from, to, relation } => {
                format!("delete_edge {from} -{}-> {to}", relation.as_str())
            }
            Self::SetEdgeAttrs {
                from, to, relation, ..
            } => format!("set_edge_attrs {from} -{}-> {to}", relation.as_str()),
            Self::SetNodeLabel { id, .. } => format!("set_node_label {id}"),
        }
    }
}

impl ProcedureGraphRecord {
    /// Apply a patch to a copy of this record: the candidate for the next
    /// version. Every op must hit an existing target (or add a new one);
    /// the result is re-validated in full, so a patch can never leave a
    /// dangling edge or an oversize graph behind. `version` is bumped and
    /// `trial_of` cleared; the caller sets provenance and the trial marker.
    pub fn apply_patch(
        &self,
        ops: &[ProcedurePatchOp],
    ) -> Result<ProcedureGraphRecord, Vec<String>> {
        if ops.is_empty() {
            return Err(vec!["a patch needs at least one op".into()]);
        }
        if ops.len() > MAX_PATCH_OPS {
            return Err(vec![format!(
                "{} ops exceeds the cap of {MAX_PATCH_OPS}",
                ops.len()
            )]);
        }
        let mut next = self.clone();
        let mut errors = Vec::new();
        for (i, op) in ops.iter().enumerate() {
            match op {
                ProcedurePatchOp::AddNode { node } => {
                    if next.nodes.iter().any(|n| n.id == node.id) {
                        errors.push(format!("op {i}: node {:?} already exists", node.id));
                    } else {
                        next.nodes.push(node.clone());
                    }
                }
                ProcedurePatchOp::DeleteNode { id } => {
                    if *id == next.entry {
                        errors.push(format!("op {i}: cannot delete the entry node {id:?}"));
                    } else if !next.nodes.iter().any(|n| n.id == *id) {
                        errors.push(format!("op {i}: node {id:?} does not exist"));
                    } else {
                        next.nodes.retain(|n| n.id != *id);
                        next.edges.retain(|e| e.from != *id && e.to != *id);
                    }
                }
                ProcedurePatchOp::AddEdge { edge } => {
                    if next.edges.iter().any(|e| {
                        e.from == edge.from && e.to == edge.to && e.relation == edge.relation
                    }) {
                        errors.push(format!(
                            "op {i}: edge {} -{}-> {} already exists",
                            edge.from,
                            edge.relation.as_str(),
                            edge.to
                        ));
                    } else {
                        next.edges.push(edge.clone());
                    }
                }
                ProcedurePatchOp::DeleteEdge { from, to, relation } => {
                    let before = next.edges.len();
                    next.edges
                        .retain(|e| !(e.from == *from && e.to == *to && e.relation == *relation));
                    if next.edges.len() == before {
                        errors.push(format!(
                            "op {i}: edge {from} -{}-> {to} does not exist",
                            relation.as_str()
                        ));
                    }
                }
                ProcedurePatchOp::SetEdgeAttrs {
                    from,
                    to,
                    relation,
                    condition,
                    guidance,
                    pitfalls,
                } => {
                    match next
                        .edges
                        .iter_mut()
                        .find(|e| e.from == *from && e.to == *to && e.relation == *relation)
                    {
                        Some(edge) => {
                            if let Some(c) = condition {
                                edge.condition = c.clone();
                            }
                            if let Some(g) = guidance {
                                edge.guidance = g.clone();
                            }
                            if let Some(p) = pitfalls {
                                edge.pitfalls = p.clone();
                            }
                        }
                        None => errors.push(format!(
                            "op {i}: edge {from} -{}-> {to} does not exist",
                            relation.as_str()
                        )),
                    }
                }
                ProcedurePatchOp::SetNodeLabel { id, label } => {
                    match next.nodes.iter_mut().find(|n| n.id == *id) {
                        Some(node) => node.label = label.clone(),
                        None => errors.push(format!("op {i}: node {id:?} does not exist")),
                    }
                }
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        next.version = self.version + 1;
        next.trial_of = None;
        next.validate()?;
        Ok(next)
    }
}

/// Lifecycle of a refiner-proposed edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProcedurePatchStatus {
    #[default]
    Pending,
    Trial,
    Accepted,
    Rejected,
}

impl ProcedurePatchStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Trial => "trial",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
        }
    }
}

/// The live trial window a candidate version runs under (the paper's
/// held-out validation, done online after operator approval).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TrialWindow {
    pub started_at: u64,
    pub candidate_version: u32,
    #[serde(default)]
    pub required_runs: usize,
    #[serde(default)]
    pub baseline_n: usize,
    #[serde(default)]
    pub baseline_mean: f32,
    #[serde(default)]
    pub candidate_n: usize,
    #[serde(default)]
    pub candidate_mean: f32,
}

/// A refiner-proposed edit and everything the gate decided about it.
///
/// Node kind: `procedure_patch`. Node key: `procedure_patch:{patch_id}`.
/// **Never deleted** — a `Rejected` patch is the paper's rejection memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProcedurePatchRecord {
    pub patch_id: String,
    pub procedure_id: String,
    /// The version the ops were written against.
    pub base_version: u32,
    /// The version the ops produced, once approved into trial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_version: Option<u32>,
    #[serde(default)]
    pub ops: Vec<ProcedurePatchOp>,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub evidence_run_ids: Vec<String>,
    /// Guest id of the proposer (the refiner whisper's session, or `phil`).
    #[serde(default)]
    pub proposed_by: String,
    #[serde(default)]
    pub status: ProcedurePatchStatus,
    /// The record as it was before approval, so a failed trial reverts to
    /// exactly what ran before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_snapshot: Option<ProcedureGraphRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial: Option<TrialWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection_reason: Option<String>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<u64>,
}

impl ProcedurePatchRecord {
    /// Every string the ops could put into a prompt, plus the rationale.
    pub fn text_fields(&self) -> Vec<&str> {
        let mut out = vec![self.rationale.as_str()];
        for op in &self.ops {
            out.extend(op.text_fields());
        }
        out
    }

    /// One line per op, for the refiner's rejection-memory rendering.
    pub fn summary(&self) -> String {
        self.ops
            .iter()
            .map(ProcedurePatchOp::summary)
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Number of candidate runs a trial needs: `PHILOTIC_PROCEDURE_TRIAL_RUNS`
/// clamped to 1..=50, default [`DEFAULT_TRIAL_RUNS`].
pub fn trial_runs_required() -> usize {
    std::env::var("PHILOTIC_PROCEDURE_TRIAL_RUNS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .map(|k| k.clamp(1, 50))
        .unwrap_or(DEFAULT_TRIAL_RUNS)
}

/// Outcome of checking a trial window against the ledger.
#[derive(Debug, Clone, PartialEq)]
pub enum TrialDecision {
    /// Fewer than `required` candidate runs so far.
    Undecided { candidate_n: usize, required: usize },
    Decided {
        accept: bool,
        candidate_n: usize,
        candidate_mean: f32,
        baseline_n: usize,
        baseline_mean: f32,
    },
}

/// The paper's gate, `S_val(G_cand) >= S_val(G_prev)`, over live runs.
///
/// `candidate` and `baseline` are newest-first score lists; the candidate
/// must have at least `required` runs, and the baseline is its newest
/// `required` runs (fewer if that is all there is). With no baseline at all
/// the bar is [`TRIAL_BAR_WITHOUT_BASELINE`]. Ties accept, as in the paper.
pub fn decide_trial(candidate: &[f32], baseline: &[f32], required: usize) -> TrialDecision {
    let required = required.max(1);
    if candidate.len() < required {
        return TrialDecision::Undecided {
            candidate_n: candidate.len(),
            required,
        };
    }
    let cand: &[f32] = &candidate[..required];
    let candidate_mean = cand.iter().sum::<f32>() / cand.len() as f32;
    let base: &[f32] = &baseline[..baseline.len().min(required)];
    let (baseline_n, baseline_mean) = if base.is_empty() {
        (0, TRIAL_BAR_WITHOUT_BASELINE)
    } else {
        (base.len(), base.iter().sum::<f32>() / base.len() as f32)
    };
    TrialDecision::Decided {
        accept: candidate_mean >= baseline_mean,
        candidate_n: cand.len(),
        candidate_mean,
        baseline_n,
        baseline_mean,
    }
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

    // ── P4 ────────────────────────────────────────────────────────────────

    #[test]
    fn apply_patch_edits_a_copy_bumps_version_and_revalidates() {
        let p = outcome_reflex_procedure();
        let ops = vec![
            ProcedurePatchOp::AddNode {
                node: tool("verify", "life.recall"),
            },
            ProcedurePatchOp::AddEdge {
                edge: ProcedureEdge {
                    from: "commit".into(),
                    to: "verify".into(),
                    relation: ProcedureRelation::LeadsTo,
                    condition: "commit returned".into(),
                    guidance: "re-read the loop to confirm loop_status".into(),
                    pitfalls: "trusting the commit reply without reading back".into(),
                    branch: None,
                },
            },
            ProcedurePatchOp::SetEdgeAttrs {
                from: "observe".into(),
                to: "commit".into(),
                relation: ProcedureRelation::LeadsTo,
                condition: None,
                guidance: Some("new guidance".into()),
                pitfalls: None,
            },
            ProcedurePatchOp::SetNodeLabel {
                id: "observe".into(),
                label: "Record it".into(),
            },
        ];
        let next = p.apply_patch(&ops).expect("applies");
        assert_eq!(next.version, p.version + 1);
        assert!(next.trial_of.is_none());
        assert_eq!(next.nodes.len(), p.nodes.len() + 1);
        assert_eq!(next.edges.len(), p.edges.len() + 1);
        assert_eq!(next.node("observe").unwrap().label, "Record it");
        let oc = next
            .edges
            .iter()
            .find(|e| e.from == "observe" && e.to == "commit")
            .unwrap();
        assert_eq!(oc.guidance, "new guidance");
        assert_eq!(oc.condition, "the outcome Event is recorded");
        // The original is untouched.
        assert_eq!(p.version, 1);
        assert!(p
            .node("observe")
            .unwrap()
            .label
            .starts_with("Record the reported"));

        // Delete a node: its edges go with it and the graph still validates.
        let next = p
            .apply_patch(&[ProcedurePatchOp::DeleteNode {
                id: "recall".into(),
            }])
            .expect("applies");
        assert!(next.node("recall").is_none());
        assert!(next
            .edges
            .iter()
            .all(|e| e.from != "recall" && e.to != "recall"));
        assert_eq!(next.validate(), Ok(()));
    }

    #[test]
    fn apply_patch_refuses_bad_targets_entry_deletion_and_invalid_results() {
        let p = outcome_reflex_procedure();
        let errors = p
            .apply_patch(&[
                ProcedurePatchOp::DeleteNode { id: "start".into() },
                ProcedurePatchOp::DeleteEdge {
                    from: "a".into(),
                    to: "b".into(),
                    relation: ProcedureRelation::LeadsTo,
                },
                ProcedurePatchOp::SetNodeLabel {
                    id: "zzz".into(),
                    label: "x".into(),
                },
                ProcedurePatchOp::AddNode {
                    node: tool("commit", "life.commit"),
                },
            ])
            .unwrap_err();
        assert_eq!(errors.len(), 4, "{errors:?}");
        assert!(errors[0].contains("entry"));
        assert!(errors[1].contains("does not exist"));
        assert!(errors[3].contains("already exists"));

        // An op set that applies but yields an invalid graph is refused too.
        let errors = p
            .apply_patch(&[ProcedurePatchOp::AddEdge {
                edge: ProcedureEdge {
                    from: "commit".into(),
                    to: "commit".into(),
                    ..Default::default()
                },
            }])
            .unwrap_err();
        assert!(errors.iter().any(|e| e.contains("self-loop")), "{errors:?}");
        assert!(p.apply_patch(&[]).is_err());
        let too_many: Vec<ProcedurePatchOp> = (0..(MAX_PATCH_OPS + 1))
            .map(|i| ProcedurePatchOp::SetNodeLabel {
                id: "commit".into(),
                label: format!("l{i}"),
            })
            .collect();
        assert!(p.apply_patch(&too_many).is_err());
    }

    #[test]
    fn trial_decision_follows_the_papers_gate() {
        // Not enough candidate runs.
        assert_eq!(
            decide_trial(&[1.0, 1.0], &[1.0], 3),
            TrialDecision::Undecided {
                candidate_n: 2,
                required: 3
            }
        );
        // Ties accept; only the newest `required` of each side count.
        match decide_trial(&[1.0, 0.0, 1.0, 0.0, 0.0], &[1.0, 0.0, 0.0, 1.0, 1.0], 3) {
            TrialDecision::Decided {
                accept,
                candidate_n,
                candidate_mean,
                baseline_n,
                baseline_mean,
            } => {
                assert!(accept);
                assert_eq!(candidate_n, 3);
                assert_eq!(baseline_n, 3);
                assert!((candidate_mean - 2.0 / 3.0).abs() < 1e-6);
                assert!((baseline_mean - 1.0 / 3.0).abs() < 1e-6);
            }
            other => panic!("{other:?}"),
        }
        // Worse than baseline rejects.
        match decide_trial(&[0.0, 0.5, 0.0], &[1.0, 1.0, 1.0], 3) {
            TrialDecision::Decided { accept, .. } => assert!(!accept),
            other => panic!("{other:?}"),
        }
        // No baseline: the bar is 0.5.
        match decide_trial(&[0.5, 0.5], &[], 2) {
            TrialDecision::Decided {
                accept,
                baseline_n,
                baseline_mean,
                ..
            } => {
                assert!(accept);
                assert_eq!(baseline_n, 0);
                assert_eq!(baseline_mean, TRIAL_BAR_WITHOUT_BASELINE);
            }
            other => panic!("{other:?}"),
        }
        match decide_trial(&[0.0, 0.5], &[], 2) {
            TrialDecision::Decided { accept, .. } => assert!(!accept),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn patch_record_round_trips_and_summarizes() {
        let rec = ProcedurePatchRecord {
            patch_id: "patch-1".into(),
            procedure_id: "outcome-reflex".into(),
            base_version: 1,
            ops: vec![
                ProcedurePatchOp::DeleteEdge {
                    from: "recall".into(),
                    to: "observe".into(),
                    relation: ProcedureRelation::LeadsTo,
                },
                ProcedurePatchOp::AddNode {
                    node: tool("verify", "life.recall"),
                },
            ],
            rationale: "the failed run skipped the read-back".into(),
            evidence_run_ids: vec!["r1".into(), "r2".into()],
            proposed_by: "agent-beacon:orchestrator".into(),
            ..Default::default()
        };
        let json = serde_json::to_value(&rec).unwrap();
        assert_eq!(json["ops"][0]["op"], "delete_edge");
        assert_eq!(json["status"], "pending");
        let back: ProcedurePatchRecord = serde_json::from_value(json).unwrap();
        assert_eq!(back, rec);
        assert_eq!(
            rec.summary(),
            "delete_edge recall -LEADS_TO-> observe; add_node verify"
        );
        assert!(rec
            .text_fields()
            .contains(&"the failed run skipped the read-back"));
        assert!(rec.text_fields().contains(&"do verify"));
    }
}
