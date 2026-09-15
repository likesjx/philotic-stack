//! LifeGraph audit: graph science over an exported snapshot of the operator's
//! graph, producing a report the gardener philote can act on one step at a
//! time — never a script the operator has to run.
//!
//! Operator direction (2026-09-12): "I want the philotes to be capable of
//! keeping the lifegraph pristine … a skill to organize, check for integrity,
//! connectedness, etc. I want to use as much graph science as possible."
//!
//! Live snapshot that motivated this (2026-09-14): 622 nodes, 225 edges, 425
//! orphans (68% of the graph disconnected), five stray bare-numeric-id
//! nodes from a MERGE bug, duplicate Person/Event nodes re-observed by a
//! looping morning sweep, and a re-observe that silently kept a stale
//! summary. None of that is visible to `life.list`'s string-prefix duplicate
//! query or the date-only hygiene timer.
//!
//! The graph is small (hundreds of nodes), so connected components, degree
//! and PageRank run here in Rust over the export and do not depend on the
//! MAGE image being present. Semantic duplicates use the embeddings the
//! runner already stores on every node. Every suggested action is atomic and
//! provenance-friendly: retire (never delete) a duplicate under a keeper,
//! link an orphan to its anchor, flag what a human must judge.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

use crate::hygiene::normalize_claim_summary;
use crate::ontology::{TERMINAL_STATUSES, VALIDATION_STATES};

/// One node as exported for the audit.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditNode {
    /// Canonical `id` property. Empty when the node has none (a defect).
    pub id: String,
    /// Memgraph internal id, for nodes without a canonical id.
    pub internal_id: i64,
    pub label: String,
    #[serde(default)]
    pub validation_state: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub observed_at: Option<String>,
    /// Best structured date (due_at / occurs_at / starts_at).
    #[serde(default)]
    pub best_date: Option<String>,
    #[serde(default)]
    pub claim_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
}

impl AuditNode {
    /// The liveness rule the ontology enforces everywhere: not retired, and
    /// no terminal status on either status property.
    pub fn is_live(&self) -> bool {
        let vs = self.validation_state.as_deref().unwrap_or("inferred");
        if vs == "retired" {
            return false;
        }
        match self.status.as_deref() {
            Some(s) => !TERMINAL_STATUSES.contains(&s),
            None => true,
        }
    }

    fn is_retirable(&self) -> bool {
        matches!(
            self.validation_state.as_deref().unwrap_or("inferred"),
            "proposed" | "inferred"
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEdge {
    pub src: String,
    pub dst: String,
    pub rel_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditOptions {
    /// ISO 8601 "now" used for staleness and past-due checks.
    pub now_iso: String,
    /// Cosine similarity at or above which two live same-label nodes are a
    /// duplicate candidate.
    pub duplicate_similarity: f32,
    /// A live loop-like node older than this with no activity is stale.
    pub stale_days: u32,
    /// Cap on suggested actions (and on each finding list).
    pub max_actions: usize,
    /// Restrict the report to these labels (empty = all).
    pub labels: Vec<String>,
}

impl Default for AuditOptions {
    fn default() -> Self {
        Self {
            now_iso: String::new(),
            duplicate_similarity: 0.90,
            stale_days: 45,
            max_actions: 25,
            labels: Vec::new(),
        }
    }
}

/// Labels that are system/telemetry records, not lived facts: never counted
/// as orphans and never proposed for linking.
pub const SYSTEM_LABELS: &[&str] = &[
    "Signal",
    "SystemPatch",
    "SkillPatch",
    "ToolPatch",
    "SchemaPatch",
    "AttentionPatch",
    "CapabilityPatch",
    "DriftFinding",
    "GrowthHypothesis",
    "GrowthExperiment",
    "ConflictHandoff",
    "OntologyExtension",
];

/// Labels that settle: a live one past its date, or untouched for
/// `stale_days`, is a stale loop.
pub const LOOP_LABELS: &[&str] = &["OpenLoop", "Commitment", "NextAction", "Goal", "Project"];

/// Labels whose meaning depends on a structured date.
pub const DATED_LABELS: &[&str] = &["Event", "Appointment", "Trip", "Commitment"];

/// Relationship types a tidy `link` may create, beyond the observe
/// vocabulary: the gardening edges.
pub const GARDENING_REL_TYPES: &[&str] = &["DUPLICATE_OF", "SUPERSEDES", "RESOLVES"];

/// One atomic, reversible-by-inspection action the gardener may take with
/// `life.tidy`. Deliberately small: retire-under-keeper, link, resolve,
/// retire. No delete exists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TidyAction {
    /// Retire `duplicate_id` (must be proposed/inferred) and record
    /// `(keeper)-[:SUPERSEDES]->(duplicate)`.
    RetireDuplicate {
        duplicate_id: String,
        keeper_id: String,
        #[serde(default)]
        reason: String,
    },
    /// MERGE `(from)-[:rel_type]->(to)` — rel_type must be in the observe or
    /// gardening vocabulary.
    Link {
        from_id: String,
        rel_type: String,
        to_id: String,
        #[serde(default)]
        reason: String,
    },
    /// Close a live loop-like node: `status = resolved`, resolved_at, note.
    Resolve {
        node_id: String,
        #[serde(default)]
        reason: String,
    },
    /// Retire a proposed/inferred node that never represented a lived fact
    /// (a stray, a test artifact). Confirmed nodes are never retired here.
    Retire {
        node_id: String,
        #[serde(default)]
        reason: String,
    },
}

impl TidyAction {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::RetireDuplicate { .. } => "retire_duplicate",
            Self::Link { .. } => "link",
            Self::Resolve { .. } => "resolve",
            Self::Retire { .. } => "retire",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LabelCount {
    pub label: String,
    pub total: usize,
    pub live: usize,
    pub live_orphans: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComponentSummary {
    pub count: usize,
    pub giant_size: usize,
    /// Live-node singletons are reported under `orphans`; components of
    /// size 2..=4 are the "islands" worth connecting.
    pub islands: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Hub {
    pub id: String,
    pub label: String,
    pub degree: usize,
    pub pagerank: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DuplicateFinding {
    pub label: String,
    pub keeper_id: String,
    pub duplicate_id: String,
    pub similarity: f32,
    pub basis: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub label: String,
    pub issue: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditReport {
    pub as_of: String,
    pub nodes: usize,
    pub edges: usize,
    pub live_nodes: usize,
    pub live_orphans: usize,
    /// 0–100. 100 = connected, no duplicates, no stale loops, no defects.
    pub health_score: u32,
    pub by_label: Vec<LabelCount>,
    pub components: ComponentSummary,
    pub hubs: Vec<Hub>,
    /// The most common SCOPED_TO target — the anchor an orphan should hang
    /// from when nothing more specific applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_role_id: Option<String>,
    pub orphans: Vec<Finding>,
    pub duplicates: Vec<DuplicateFinding>,
    pub stale_loops: Vec<Finding>,
    pub temporal_issues: Vec<Finding>,
    pub conformance_issues: Vec<Finding>,
    /// Atomic actions, one `life.tidy` call each, in priority order.
    pub suggested_actions: Vec<TidyAction>,
    /// Findings that need a judgment call (stale loops, islands): the
    /// gardener asks the operator or resolves with evidence, never guesses.
    pub needs_judgment: Vec<String>,
}

struct UnionFind {
    parent: Vec<usize>,
    size: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            size: vec![1; n],
        }
    }
    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        let (mut ra, mut rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        if self.size[ra] < self.size[rb] {
            std::mem::swap(&mut ra, &mut rb);
        }
        self.parent[rb] = ra;
        self.size[ra] += self.size[rb];
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Days between two ISO 8601 timestamps (date prefix compared), or None.
fn days_between(earlier: &str, later: &str) -> Option<i64> {
    let e = chrono::NaiveDate::parse_from_str(earlier.get(..10)?, "%Y-%m-%d").ok()?;
    let l = chrono::NaiveDate::parse_from_str(later.get(..10)?, "%Y-%m-%d").ok()?;
    Some((l - e).num_days())
}

/// Confirmed beats proposed; then the newer `observed_at`; then the one
/// with a canonical id.
fn prefer_keeper<'a>(a: &'a AuditNode, b: &'a AuditNode) -> (&'a AuditNode, &'a AuditNode) {
    let rank = |n: &AuditNode| -> (u8, String, u8) {
        (
            u8::from(n.validation_state.as_deref() == Some("confirmed")),
            n.observed_at.clone().unwrap_or_default(),
            u8::from(!n.id.is_empty()),
        )
    };
    if rank(b) > rank(a) { (b, a) } else { (a, b) }
}

/// Run the audit over an exported snapshot.
pub fn audit(nodes: &[AuditNode], edges: &[AuditEdge], opts: &AuditOptions) -> AuditReport {
    let now = opts.now_iso.clone();
    let in_scope = |n: &AuditNode| opts.labels.is_empty() || opts.labels.contains(&n.label);

    // Index by canonical id, falling back to the internal id for strays.
    let key_of = |n: &AuditNode| -> String {
        if n.id.is_empty() {
            format!("#{}", n.internal_id)
        } else {
            n.id.clone()
        }
    };
    let index: HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (key_of(n), i))
        .collect();

    // Degree + components over the whole graph (scope filters only what is
    // reported, not what is connected).
    let mut degree = vec![0usize; nodes.len()];
    let mut uf = UnionFind::new(nodes.len());
    let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    let mut scoped_to_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut known_rel_types: Vec<&str> = crate::cypher::LIVING_CYCLE_REL_TYPES.to_vec();
    known_rel_types.extend(crate::cypher::agenda_rel_types());
    known_rel_types.extend(GARDENING_REL_TYPES);
    let mut conformance: Vec<Finding> = Vec::new();
    for e in edges {
        if let (Some(&a), Some(&b)) = (index.get(&e.src), index.get(&e.dst)) {
            degree[a] += 1;
            degree[b] += 1;
            adjacency[a].push(b);
            adjacency[b].push(a);
            uf.union(a, b);
        }
        if e.rel_type == "SCOPED_TO" {
            *scoped_to_counts.entry(e.dst.clone()).or_default() += 1;
        }
        if !known_rel_types.contains(&e.rel_type.as_str()) {
            conformance.push(Finding {
                id: e.src.clone(),
                label: "edge".into(),
                issue: "unknown_rel_type".into(),
                detail: Some(format!("{} -[{}]-> {}", e.src, e.rel_type, e.dst)),
            });
        }
    }
    let anchor_role_id = scoped_to_counts
        .iter()
        .max_by_key(|(_, c)| **c)
        .map(|(id, _)| id.clone());

    // PageRank (undirected, damping 0.85, 25 iterations) — small graph.
    let n = nodes.len().max(1);
    let mut pr = vec![1.0 / n as f64; nodes.len()];
    for _ in 0..25 {
        let mut next = vec![0.15 / n as f64; nodes.len()];
        for (i, nbrs) in adjacency.iter().enumerate() {
            if nbrs.is_empty() {
                continue;
            }
            let share = 0.85 * pr[i] / nbrs.len() as f64;
            for &j in nbrs {
                next[j] += share;
            }
        }
        pr = next;
    }

    // Components.
    let mut comp_members: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..nodes.len() {
        comp_members.entry(uf.find(i)).or_default().push(i);
    }
    let mut sizes: Vec<usize> = comp_members.values().map(Vec::len).collect();
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    // Islands: small components that are not the giant one.
    let giant_root = comp_members
        .iter()
        .max_by_key(|(_, m)| m.len())
        .map(|(root, _)| *root);
    let mut islands: Vec<Vec<String>> = comp_members
        .iter()
        .filter(|(root, m)| {
            Some(**root) != giant_root
                && (2..=4).contains(&m.len())
                && m.iter().any(|&i| nodes[i].is_live())
        })
        .map(|(_, m)| m.iter().map(|&i| key_of(&nodes[i])).collect())
        .collect();
    islands.sort();
    islands.truncate(opts.max_actions);

    // Per-label counts and orphans.
    let mut by_label: BTreeMap<String, LabelCount> = BTreeMap::new();
    let mut orphans: Vec<Finding> = Vec::new();
    let mut live_nodes = 0usize;
    let mut live_orphans = 0usize;
    for (i, node) in nodes.iter().enumerate() {
        let entry = by_label
            .entry(node.label.clone())
            .or_insert_with(|| LabelCount {
                label: node.label.clone(),
                ..Default::default()
            });
        entry.total += 1;
        if node.is_live() {
            entry.live += 1;
            live_nodes += 1;
            if degree[i] == 0 && !SYSTEM_LABELS.contains(&node.label.as_str()) {
                entry.live_orphans += 1;
                live_orphans += 1;
                if in_scope(node) {
                    orphans.push(Finding {
                        id: key_of(node),
                        label: node.label.clone(),
                        issue: "orphan".into(),
                        detail: node
                            .claim_summary
                            .clone()
                            .map(|s| s.chars().take(120).collect()),
                    });
                }
            }
        }
    }

    // Hubs.
    let mut hubs: Vec<Hub> = nodes
        .iter()
        .enumerate()
        .filter(|(i, _)| degree[*i] > 0)
        .map(|(i, nd)| Hub {
            id: key_of(nd),
            label: nd.label.clone(),
            degree: degree[i],
            pagerank: pr[i],
        })
        .collect();
    hubs.sort_by(|a, b| {
        b.pagerank
            .partial_cmp(&a.pagerank)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hubs.truncate(10);

    // Duplicates: live, same label, in scope; exact normalized summary OR
    // embedding cosine above threshold. Pairwise within label groups.
    let mut duplicates: Vec<DuplicateFinding> = Vec::new();
    let mut by_label_live: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, nd) in nodes.iter().enumerate() {
        if nd.is_live() && in_scope(nd) && !SYSTEM_LABELS.contains(&nd.label.as_str()) {
            by_label_live.entry(nd.label.as_str()).or_default().push(i);
        }
    }
    let mut already_dup: Vec<String> = Vec::new();
    for (label, members) in &by_label_live {
        for (x, &i) in members.iter().enumerate() {
            for &j in &members[x + 1..] {
                let (a, b) = (&nodes[i], &nodes[j]);
                let norm_a = a.claim_summary.as_deref().map(normalize_claim_summary);
                let norm_b = b.claim_summary.as_deref().map(normalize_claim_summary);
                let exact = norm_a.is_some() && norm_a == norm_b;
                let sim = match (&a.embedding, &b.embedding) {
                    (Some(ea), Some(eb)) => cosine(ea, eb),
                    _ => 0.0,
                };
                if !(exact || sim >= opts.duplicate_similarity) {
                    continue;
                }
                let (keeper, dup) = prefer_keeper(a, b);
                let dup_key = key_of(dup);
                if already_dup.contains(&dup_key) || !dup.is_retirable() {
                    continue;
                }
                already_dup.push(dup_key.clone());
                duplicates.push(DuplicateFinding {
                    label: (*label).to_string(),
                    keeper_id: key_of(keeper),
                    duplicate_id: dup_key,
                    similarity: if exact { 1.0 } else { sim },
                    basis: if exact {
                        "exact_summary".into()
                    } else {
                        "embedding".into()
                    },
                });
            }
        }
    }
    duplicates.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    duplicates.truncate(opts.max_actions);

    // Stale loops and temporal issues.
    let mut stale: Vec<Finding> = Vec::new();
    let mut temporal: Vec<Finding> = Vec::new();
    for nd in nodes.iter().filter(|n| in_scope(n)) {
        let label = nd.label.as_str();
        if nd.is_live() && LOOP_LABELS.contains(&label) {
            if let Some(d) = nd.best_date.as_deref()
                && let Some(days) = days_between(d, &now)
                && days > 1
            {
                stale.push(Finding {
                    id: key_of(nd),
                    label: nd.label.clone(),
                    issue: "past_due".into(),
                    detail: Some(format!("{days} days past {d}")),
                });
                continue;
            }
            if let Some(o) = nd.observed_at.as_deref()
                && let Some(days) = days_between(o, &now)
                && days >= i64::from(opts.stale_days)
            {
                stale.push(Finding {
                    id: key_of(nd),
                    label: nd.label.clone(),
                    issue: "untouched".into(),
                    detail: Some(format!("{days} days since observed")),
                });
            }
        }
        if DATED_LABELS.contains(&label) && nd.best_date.is_none() && nd.is_live() {
            temporal.push(Finding {
                id: key_of(nd),
                label: nd.label.clone(),
                issue: "missing_date".into(),
                detail: None,
            });
        }
        if nd.observed_at.is_none() && !SYSTEM_LABELS.contains(&label) && nd.label != "Role" {
            temporal.push(Finding {
                id: key_of(nd),
                label: nd.label.clone(),
                issue: "missing_observed_at".into(),
                detail: None,
            });
        }
    }
    stale.truncate(opts.max_actions);
    temporal.truncate(opts.max_actions);

    // Conformance: ids and validation states.
    for nd in nodes.iter().filter(|n| in_scope(n)) {
        if nd.id.is_empty() {
            conformance.push(Finding {
                id: key_of(nd),
                label: nd.label.clone(),
                issue: "missing_id".into(),
                detail: nd.claim_summary.clone(),
            });
        } else if nd.id.bytes().all(|b| b.is_ascii_digit()) {
            conformance.push(Finding {
                id: nd.id.clone(),
                label: nd.label.clone(),
                issue: "bare_numeric_id".into(),
                detail: nd.claim_summary.clone(),
            });
        }
        if let Some(vs) = nd.validation_state.as_deref()
            && !VALIDATION_STATES.contains(&vs)
        {
            conformance.push(Finding {
                id: key_of(nd),
                label: nd.label.clone(),
                issue: "unknown_validation_state".into(),
                detail: Some(vs.to_string()),
            });
        }
    }
    conformance.truncate(opts.max_actions);

    // Suggested actions, deterministic ones only, priority order:
    // duplicates first (they pollute recall), then stray numeric ids, then
    // orphan scoping to the anchor role.
    let mut actions: Vec<TidyAction> = Vec::new();
    for d in &duplicates {
        actions.push(TidyAction::RetireDuplicate {
            duplicate_id: d.duplicate_id.clone(),
            keeper_id: d.keeper_id.clone(),
            reason: format!(
                "duplicate of {} ({} {:.2})",
                d.keeper_id, d.basis, d.similarity
            ),
        });
    }
    for c in conformance.iter().filter(|c| c.issue == "bare_numeric_id") {
        actions.push(TidyAction::Retire {
            node_id: c.id.clone(),
            reason: "bare numeric id: manufactured by a commit on an internal id, not a lived fact"
                .into(),
        });
    }
    if let Some(anchor) = anchor_role_id.as_deref() {
        // Only canonical `life:` records are anchored. A node under another
        // scheme ("ontology:extensions", "pref-bjork-…") is a conformance
        // problem, not a lived fact to scope — and a link step naming it
        // carries no id of its own for the plan evaluator to prove (live
        // 2026-09-15 18:30 UTC, DEF-137).
        for o in orphans.iter().filter(|o| o.id.starts_with("life:")) {
            actions.push(TidyAction::Link {
                from_id: o.id.clone(),
                rel_type: "SCOPED_TO".into(),
                to_id: anchor.to_string(),
                reason: "orphan: attach to the operator's anchor role so it is reachable".into(),
            });
        }
    }
    actions.truncate(opts.max_actions);

    let mut needs_judgment: Vec<String> = Vec::new();
    for o in orphans
        .iter()
        .filter(|o| !o.id.starts_with("life:") && !o.id.starts_with('#'))
    {
        needs_judgment.push(format!(
            "{} ({}) is an unlinked node outside the life: id scheme — register its shape via life.patch.propose, re-observe it under a canonical id, or retire it",
            o.id, o.label
        ));
    }
    for s in &stale {
        needs_judgment.push(format!(
            "{} ({}) is {}: {} — resolve with the outcome, re-date it, or confirm it is still open",
            s.id,
            s.label,
            s.issue,
            s.detail.clone().unwrap_or_default()
        ));
    }
    for island in &islands {
        needs_judgment.push(format!(
            "island of {} nodes not reachable from the main graph: {}",
            island.len(),
            island.join(", ")
        ));
    }

    let nodes_total = nodes.len();
    // Health: each category is capped so one noisy class (20 legacy custom
    // edges, live 2026-09-14, scored a healthy graph 0) cannot saturate the
    // score. Distinct unknown edge TYPES count, not every edge of that type —
    // a consistent custom vocabulary is a registration gap, not 20 defects.
    let orphan_ratio = if live_nodes == 0 {
        0.0
    } else {
        live_orphans as f64 / live_nodes as f64
    };
    let unknown_types: std::collections::BTreeSet<String> = conformance
        .iter()
        .filter(|c| c.issue == "unknown_rel_type")
        .filter_map(|c| c.detail.as_deref())
        .filter_map(|d| d.split("-[").nth(1).and_then(|r| r.split("]->").next()))
        .map(str::to_string)
        .collect();
    let other_conformance = conformance
        .iter()
        .filter(|c| c.issue != "unknown_rel_type")
        .count();
    let penalty = (orphan_ratio * 60.0).min(30.0)
        + (duplicates.len() as f64 * 2.0).min(25.0)
        + (stale.len() as f64).min(15.0)
        + (temporal.len() as f64 * 0.5).min(10.0)
        + (unknown_types.len() as f64 * 3.0 + other_conformance as f64 * 2.0).min(20.0);
    let health_score = (100.0 - penalty).clamp(0.0, 100.0).round() as u32;
    if !unknown_types.is_empty() {
        needs_judgment.push(format!(
            "edge types outside the vocabulary: {} — register them as ontology extensions with \
             life.patch.propose (or the operator confirms them); do not rewire existing edges",
            unknown_types.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }

    AuditReport {
        as_of: now,
        nodes: nodes_total,
        edges: edges.len(),
        live_nodes,
        live_orphans,
        health_score,
        by_label: by_label.into_values().collect(),
        components: ComponentSummary {
            count: sizes.len(),
            giant_size: sizes.first().copied().unwrap_or(0),
            islands,
        },
        hubs,
        anchor_role_id,
        orphans,
        duplicates,
        stale_loops: stale,
        temporal_issues: temporal,
        conformance_issues: conformance,
        suggested_actions: actions,
        needs_judgment,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, label: &str, vs: &str) -> AuditNode {
        AuditNode {
            id: id.into(),
            internal_id: 0,
            label: label.into(),
            validation_state: Some(vs.into()),
            observed_at: Some("2026-09-01T00:00:00Z".into()),
            ..Default::default()
        }
    }
    fn edge(a: &str, rel: &str, b: &str) -> AuditEdge {
        AuditEdge {
            src: a.into(),
            dst: b.into(),
            rel_type: rel.into(),
        }
    }
    fn opts() -> AuditOptions {
        AuditOptions {
            now_iso: "2026-09-14T12:00:00Z".into(),
            ..Default::default()
        }
    }

    #[test]
    fn components_orphans_and_anchor() {
        let nodes = vec![
            node("life:role:chief-of-staff", "Role", "confirmed"),
            node("life:person:nadi", "Person", "proposed"),
            node("life:person:gabby", "Person", "proposed"),
            node("life:place:home", "Place", "proposed"),
            node("life:signal:fb1", "Signal", "confirmed"),
            node("life:event:a", "Event", "proposed"),
            node("life:event:b", "Event", "proposed"),
        ];
        let edges = vec![
            edge("life:person:nadi", "SCOPED_TO", "life:role:chief-of-staff"),
            edge("life:person:gabby", "SCOPED_TO", "life:role:chief-of-staff"),
            edge("life:event:a", "RELATES_TO", "life:event:b"),
        ];
        let r = audit(&nodes, &edges, &opts());
        assert_eq!(
            r.anchor_role_id.as_deref(),
            Some("life:role:chief-of-staff")
        );
        // home is a live orphan; the Signal is not counted.
        assert_eq!(r.live_orphans, 1);
        assert_eq!(r.orphans[0].id, "life:place:home");
        assert_eq!(r.components.giant_size, 3);
        assert_eq!(
            r.components.islands,
            vec![vec!["life:event:a".to_string(), "life:event:b".to_string()]]
        );
        assert!(r.hubs[0].id == "life:role:chief-of-staff");
        assert!(
            matches!(&r.suggested_actions[0], TidyAction::Link { from_id, rel_type, to_id, .. }
            if from_id == "life:place:home" && rel_type == "SCOPED_TO" && to_id == "life:role:chief-of-staff")
        );
        assert!(r.needs_judgment.iter().any(|s| s.contains("island of 2")));
    }

    /// Live 2026-09-15 18:30 UTC: "ontology:extensions" and
    /// "pref-bjork-manages-musician-roles" were proposed as SCOPED_TO links.
    #[test]
    fn non_life_ids_are_never_anchored_only_flagged() {
        let nodes = vec![
            node("life:role:chief-of-staff", "Role", "confirmed"),
            node("life:place:home", "Place", "confirmed"),
            node(
                "pref-bjork-manages-musician-roles",
                "Preference",
                "confirmed",
            ),
            node("ontology:extensions", "OntologyExtension", "confirmed"),
            node("life:goal:g", "Goal", "confirmed"),
        ];
        let edges = vec![edge("life:goal:g", "SCOPED_TO", "life:role:chief-of-staff")];
        let r = audit(&nodes, &edges, &AuditOptions::default());
        assert!(
            r.suggested_actions.iter().all(
                |a| !matches!(a, TidyAction::Link { from_id, .. } if !from_id.starts_with("life:"))
            ),
            "{:?}",
            r.suggested_actions
        );
        assert!(r.suggested_actions.iter().any(
            |a| matches!(a, TidyAction::Link { from_id, .. } if from_id == "life:place:home")
        ));
        assert!(
            r.needs_judgment
                .iter()
                .any(|s| s.starts_with("pref-bjork-manages-musician-roles")),
            "{:?}",
            r.needs_judgment
        );
        // The ontology record is a system node: not an orphan at all.
        assert!(r.orphans.iter().all(|o| o.id != "ontology:extensions"));
    }

    /// The 2026-09-12 morning-sweep duplicates: same summary text re-observed
    /// under a new id. Keeper is the older (first) node when neither is
    /// confirmed? No — newer observed_at wins per the hygiene rule, but a
    /// confirmed node always wins.
    #[test]
    fn duplicates_by_exact_summary_and_embedding() {
        let mut a = node(
            "life:event:drive_daxton_school_20260910",
            "Event",
            "confirmed",
        );
        a.claim_summary = Some("Jared drove Daxton to school on Thursday morning.".into());
        a.observed_at = Some("2026-09-10T11:04:00Z".into());
        let mut b = node(
            "life:event:school_drive_daxton_20260910",
            "Event",
            "proposed",
        );
        b.claim_summary = Some("jared drove daxton to school on thursday morning".into());
        b.observed_at = Some("2026-09-11T11:00:00Z".into());
        let mut c = node("life:person:daxton", "Person", "proposed");
        c.embedding = Some(vec![1.0, 0.0, 0.0]);
        let mut d = node("life:person:daxton_thomas_likes", "Person", "proposed");
        d.embedding = Some(vec![0.98, 0.05, 0.0]);
        d.observed_at = Some("2026-08-26T00:00:00Z".into());
        let mut e = node("life:person:gabby", "Person", "proposed");
        e.embedding = Some(vec![0.0, 1.0, 0.0]);
        let r = audit(&[a, b, c, d, e], &[], &opts());
        let pairs: Vec<(&str, &str, &str)> = r
            .duplicates
            .iter()
            .map(|f| {
                (
                    f.keeper_id.as_str(),
                    f.duplicate_id.as_str(),
                    f.basis.as_str(),
                )
            })
            .collect();
        assert!(
            pairs.contains(&(
                "life:event:drive_daxton_school_20260910",
                "life:event:school_drive_daxton_20260910",
                "exact_summary"
            )),
            "{pairs:?}"
        );
        // Newer observed_at keeps: life:person:daxton is newer than the 08-26 node.
        assert!(
            pairs.contains(&(
                "life:person:daxton",
                "life:person:daxton_thomas_likes",
                "embedding"
            )),
            "{pairs:?}"
        );
        assert_eq!(pairs.len(), 2);
        assert!(
            r.suggested_actions
                .iter()
                .all(|a| matches!(a, TidyAction::RetireDuplicate { .. }))
        );
    }

    #[test]
    fn stale_temporal_and_conformance_findings() {
        let mut past = node("life:commitment:x", "Commitment", "proposed");
        past.best_date = Some("2026-08-01".into());
        let mut old = node("life:open_loop:y", "OpenLoop", "proposed");
        old.observed_at = Some("2026-06-01T00:00:00Z".into());
        let mut undated = node("life:event:z", "Event", "proposed");
        undated.best_date = None;
        let stray = AuditNode {
            id: "557".into(),
            internal_id: 590,
            label: "Commitment".into(),
            validation_state: Some("confirmed".into()),
            ..Default::default()
        };
        let mut done = node("life:open_loop:done", "OpenLoop", "confirmed");
        done.status = Some("resolved".into());
        done.observed_at = Some("2026-01-01T00:00:00Z".into());
        let r = audit(&[past, old, undated, stray, done], &[], &opts());
        let issues: Vec<(&str, &str)> = r
            .stale_loops
            .iter()
            .map(|f| (f.id.as_str(), f.issue.as_str()))
            .collect();
        assert!(
            issues.contains(&("life:commitment:x", "past_due")),
            "{issues:?}"
        );
        assert!(
            issues.contains(&("life:open_loop:y", "untouched")),
            "{issues:?}"
        );
        assert!(
            !issues.iter().any(|(id, _)| *id == "life:open_loop:done"),
            "resolved loops are not stale"
        );
        assert!(
            r.temporal_issues
                .iter()
                .any(|f| f.id == "life:event:z" && f.issue == "missing_date")
        );
        assert!(
            r.conformance_issues
                .iter()
                .any(|f| f.id == "557" && f.issue == "bare_numeric_id")
        );
        assert!(
            r.suggested_actions
                .iter()
                .any(|a| matches!(a, TidyAction::Retire { node_id, .. } if node_id == "557"))
        );
        assert!(
            r.needs_judgment
                .iter()
                .any(|s| s.contains("life:commitment:x"))
        );
        assert!(r.health_score < 100);
    }

    #[test]
    fn pristine_graph_scores_100() {
        let nodes = vec![
            node("life:role:r", "Role", "confirmed"),
            node("life:person:p", "Person", "confirmed"),
        ];
        let edges = vec![edge("life:person:p", "SCOPED_TO", "life:role:r")];
        let r = audit(&nodes, &edges, &opts());
        assert_eq!(r.health_score, 100);
        assert!(r.suggested_actions.is_empty());
    }
}
