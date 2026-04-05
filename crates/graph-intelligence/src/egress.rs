use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use chrono::Utc;

use crate::engine::GraphEngine;
use crate::schema::{EdgeRelation, NodeKind};
use crate::writeback::update_frontmatter;

// ── graph_export_docs ─────────────────────────────────────────────────────────

/// Sync graph state back to documentation files on disk.
///
/// For every doc-kind node that has a `file_path`, writes the current graph
/// values for `status`, `disposition`, `domain`, `tags`, and `last_updated`
/// back into that file's YAML frontmatter.  Also derives `active_seams` from
/// outgoing Implements / References edges and writes that list too.
///
/// Returns a report of every file updated.
pub fn export_docs(engine: &GraphEngine, repo_root: &Path) -> Result<ExportDocsReport> {
    let mut updated: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    let doc_kinds = [
        NodeKind::Proposal,
        NodeKind::Seam,
        NodeKind::Task,
        NodeKind::Domain,
        NodeKind::Document,
        NodeKind::Skill,
        NodeKind::Sver,
    ];

    for kind in &doc_kinds {
        let nodes = engine.query_nodes(Some(*kind), None)?;
        for node in nodes {
            let file_path_str = match node.file_path.as_deref() {
                Some(p) if !p.is_empty() => p,
                _ => {
                    skipped.push(format!("{} (no file_path)", node.id));
                    continue;
                }
            };

            // Resolve path — may be absolute or relative to repo root
            let file_path = if Path::new(file_path_str).is_absolute() {
                std::path::PathBuf::from(file_path_str)
            } else {
                repo_root.join(file_path_str)
            };

            if !file_path.exists() {
                skipped.push(format!(
                    "{} (file not found: {})",
                    node.id,
                    file_path.display()
                ));
                continue;
            }

            let mut updates: HashMap<String, serde_yaml::Value> = HashMap::new();

            // Core lifecycle fields
            if let Some(v) = node.properties.get("status").and_then(|v| v.as_str()) {
                updates.insert("status".into(), serde_yaml::Value::String(v.to_string()));
            }
            if let Some(v) = node.properties.get("disposition").and_then(|v| v.as_str()) {
                updates.insert(
                    "disposition".into(),
                    serde_yaml::Value::String(v.to_string()),
                );
            }
            if let Some(v) = node.properties.get("domain").and_then(|v| v.as_str()) {
                updates.insert("domain".into(), serde_yaml::Value::String(v.to_string()));
            }

            // Tags — stored as comma-separated string or JSON array
            if let Some(tags_val) = node.properties.get("tags") {
                let tag_list: Vec<serde_yaml::Value> = if let Some(s) = tags_val.as_str() {
                    s.split(',')
                        .map(|t| serde_yaml::Value::String(t.trim().to_string()))
                        .filter(|v| matches!(v, serde_yaml::Value::String(s) if !s.is_empty()))
                        .collect()
                } else if let Some(arr) = tags_val.as_array() {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| serde_yaml::Value::String(s.to_string()))
                        .collect()
                } else {
                    vec![]
                };
                if !tag_list.is_empty() {
                    updates.insert("tags".into(), serde_yaml::Value::Sequence(tag_list));
                }
            }

            // Derive active_seams from edges
            let edges = engine.get_edges_from(&node.id)?;
            let seam_ids: Vec<serde_yaml::Value> = edges
                .iter()
                .filter(|e| {
                    matches!(
                        e.relation,
                        EdgeRelation::Implements | EdgeRelation::References | EdgeRelation::Governs
                    )
                })
                .filter(|e| e.target_id.starts_with("seam:"))
                .map(|e| serde_yaml::Value::String(e.target_id.clone()))
                .collect();
            if !seam_ids.is_empty() {
                updates.insert("active_seams".into(), serde_yaml::Value::Sequence(seam_ids));
            }

            // Verification level
            if let Some(v) = node
                .properties
                .get("verification_level")
                .and_then(|v| v.as_str())
            {
                updates.insert(
                    "verification_level".into(),
                    serde_yaml::Value::String(v.to_string()),
                );
            }

            // Stamp last_updated
            updates.insert(
                "last_updated".into(),
                serde_yaml::Value::String(Utc::now().format("%Y-%m-%d").to_string()),
            );

            if updates.is_empty() {
                skipped.push(format!("{} (no graph fields to write)", node.id));
                continue;
            }

            match update_frontmatter(&file_path, &updates) {
                Ok(()) => updated.push(format!("{} → {}", node.id, file_path.display())),
                Err(e) => errors.push(format!("{}: {}", node.id, e)),
            }
        }
    }

    Ok(ExportDocsReport {
        updated,
        skipped,
        errors,
    })
}

pub struct ExportDocsReport {
    pub updated: Vec<String>,
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
}

// ── graph_export_sver ─────────────────────────────────────────────────────────

/// Generate a fresh SVER markdown document from the graph's current
/// verification state.  Returns the markdown string — caller decides
/// whether to write it to a file.
pub fn export_sver_doc(engine: &GraphEngine) -> Result<String> {
    let mut out = String::new();

    let now = Utc::now().format("%Y-%m-%d").to_string();

    // Frontmatter
    out.push_str("---\n");
    out.push_str("doc_type: sver\n");
    out.push_str("title: SVER — Verification Ladder State\n");
    out.push_str(&format!("last_updated: {}\n", now));
    out.push_str("status: active\n");
    out.push_str("---\n\n");

    out.push_str("# SVER — Verification Ladder State\n\n");
    out.push_str(&format!("> Auto-generated from graph on {}.  \n", now));
    out.push_str(
        "> Source of truth: the graph.  Do not hand-edit — rerun `graph_export_sver`.\n\n",
    );

    // ── Per-domain proposal table ──────────────────────────────────────────
    out.push_str("## Proposals by Domain\n\n");

    let proposals = engine.query_nodes(Some(NodeKind::Proposal), None)?;
    let mut by_domain: HashMap<String, Vec<_>> = HashMap::new();
    for p in &proposals {
        let domain = p
            .properties
            .get("domain")
            .and_then(|v| v.as_str())
            .unwrap_or("unassigned")
            .to_string();
        by_domain.entry(domain).or_default().push(p);
    }

    let mut domains: Vec<&String> = by_domain.keys().collect();
    domains.sort();

    for domain in domains {
        let props = &by_domain[domain];
        out.push_str(&format!("### {}\n\n", domain));
        out.push_str("| Proposal | Disposition | Verification | Seams |\n");
        out.push_str("|---|---|---|---|\n");

        for p in props {
            let disposition = p
                .properties
                .get("disposition")
                .and_then(|v| v.as_str())
                .unwrap_or("—");
            let verification = p
                .properties
                .get("verification_level")
                .and_then(|v| v.as_str())
                .unwrap_or("none");

            // Collect seams this proposal implements
            let edges = engine.get_edges_from(&p.id)?;
            let seams: Vec<&str> = edges
                .iter()
                .filter(|e| {
                    e.relation == EdgeRelation::Implements && e.target_id.starts_with("seam:")
                })
                .map(|e| e.target_id.as_str())
                .collect();
            let seams_str = if seams.is_empty() {
                "—".to_string()
            } else {
                seams.join(", ")
            };

            out.push_str(&format!(
                "| `{}` {} | {} | `{}` | {} |\n",
                p.id, p.name, disposition, verification, seams_str
            ));
        }
        out.push('\n');
    }

    // ── Active seams ──────────────────────────────────────────────────────
    out.push_str("## Active Seams\n\n");
    let seams = engine.query_nodes(Some(NodeKind::Seam), None)?;
    if seams.is_empty() {
        out.push_str("_No seams registered._\n\n");
    } else {
        out.push_str("| Seam | Status | Blocks |\n");
        out.push_str("|---|---|---|\n");
        for seam in &seams {
            let status = seam
                .properties
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("—");
            let edges = engine.get_edges_from(&seam.id)?;
            let blocks: Vec<&str> = edges
                .iter()
                .filter(|e| e.relation == EdgeRelation::Blocks)
                .map(|e| e.target_id.as_str())
                .collect();
            let blocks_str = if blocks.is_empty() {
                "—".to_string()
            } else {
                blocks.join(", ")
            };
            out.push_str(&format!(
                "| `{}` | {} | {} |\n",
                seam.id, status, blocks_str
            ));
        }
        out.push('\n');
    }

    // ── Verification runs ─────────────────────────────────────────────────
    out.push_str("## Recent Verification Runs\n\n");
    let test_runs = engine.query_nodes(Some(NodeKind::TestRun), None)?;
    let smoke_runs = engine.query_nodes(Some(NodeKind::SmokeRun), None)?;
    let uat_runs = engine.query_nodes(Some(NodeKind::UatRun), None)?;

    let mut all_runs: Vec<_> = test_runs
        .iter()
        .chain(smoke_runs.iter())
        .chain(uat_runs.iter())
        .collect();

    // Sort by created_at descending
    all_runs.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    if all_runs.is_empty() {
        out.push_str("_No verification runs recorded._\n\n");
    } else {
        out.push_str("| Run | Kind | Status | Target | Time |\n");
        out.push_str("|---|---|---|---|---|\n");
        for run in all_runs.iter().take(30) {
            let kind_str = run.kind.as_str();
            let status = run
                .properties
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("—");
            let target = run
                .properties
                .get("target_node")
                .and_then(|v| v.as_str())
                .unwrap_or("—");
            let time = run.created_at.format("%Y-%m-%d %H:%M").to_string();
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} |\n",
                run.id, kind_str, status, target, time
            ));
        }
        out.push('\n');
    }

    // ── Skills ────────────────────────────────────────────────────────────
    out.push_str("## Registered Skills\n\n");
    let skills = engine.query_nodes(Some(NodeKind::Skill), None)?;
    if skills.is_empty() {
        out.push_str("_No skills indexed._\n\n");
    } else {
        for skill in &skills {
            let desc = skill
                .properties
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            out.push_str(&format!("- **`{}`** — {}\n", skill.name, desc));
        }
        out.push('\n');
    }

    Ok(out)
}

// ── graph_digest ──────────────────────────────────────────────────────────────

/// Generate a compressed architecture digest — domain→proposal→seam→verification
/// chain.  Designed to give any agent a complete picture in one round-trip,
/// with minimal tokens.
pub fn generate_digest(engine: &GraphEngine) -> Result<DigestReport> {
    let proposals = engine.query_nodes(Some(NodeKind::Proposal), None)?;
    let seams = engine.query_nodes(Some(NodeKind::Seam), None)?;
    let skills = engine.query_nodes(Some(NodeKind::Skill), None)?;
    let sessions = engine.query_nodes(Some(NodeKind::Session), None)?;
    let decisions = engine.query_nodes(Some(NodeKind::Decision), None)?;

    // Total counts
    let total_nodes = engine.count_nodes(None)?;
    let total_edges = engine.count_edges()?;

    // Build domain chains
    let mut by_domain: HashMap<String, Vec<DomainProposalEntry>> = HashMap::new();
    for p in &proposals {
        let domain = p
            .properties
            .get("domain")
            .and_then(|v| v.as_str())
            .unwrap_or("unassigned")
            .to_string();

        let disposition = p
            .properties
            .get("disposition")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let status = p
            .properties
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let verification = p
            .properties
            .get("verification_level")
            .and_then(|v| v.as_str())
            .unwrap_or("none")
            .to_string();

        // Seams this proposal implements
        let edges = engine.get_edges_from(&p.id)?;
        let active_seams: Vec<String> = edges
            .iter()
            .filter(|e| {
                matches!(
                    e.relation,
                    EdgeRelation::Implements | EdgeRelation::References
                ) && e.target_id.starts_with("seam:")
            })
            .map(|e| e.target_id.clone())
            .collect();

        by_domain
            .entry(domain)
            .or_default()
            .push(DomainProposalEntry {
                id: p.id.clone(),
                name: p.name.clone(),
                disposition,
                status,
                verification,
                active_seams,
            });
    }

    // Active seams (those without a status of "closed")
    let active_seams: Vec<SeamEntry> = seams
        .iter()
        .filter(|s| {
            s.properties
                .get("status")
                .and_then(|v| v.as_str())
                .map(|s| s != "closed")
                .unwrap_or(true)
        })
        .map(|s| SeamEntry {
            id: s.id.clone(),
            name: s.name.clone(),
            status: s
                .properties
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("open")
                .to_string(),
        })
        .collect();

    // Active sessions
    let active_sessions: Vec<SessionEntry> = sessions
        .iter()
        .filter(|s| {
            s.properties
                .get("status")
                .and_then(|v| v.as_str())
                .map(|s| s == "active")
                .unwrap_or(false)
        })
        .map(|s| SessionEntry {
            id: s.id.clone(),
            agent: s
                .properties
                .get("agent")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            seam_id: s
                .properties
                .get("seam_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            phase: s
                .properties
                .get("phase")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
        })
        .collect();

    // Recent decisions (last 10)
    let mut recent_decisions: Vec<DecisionEntry> = decisions
        .iter()
        .map(|d| DecisionEntry {
            id: d.id.clone(),
            name: d.name.clone(),
            at: d.created_at,
        })
        .collect();
    recent_decisions.sort_by(|a, b| b.at.cmp(&a.at));
    recent_decisions.truncate(10);

    // Sort domains
    let mut domains: Vec<DomainChain> = by_domain
        .into_iter()
        .map(|(name, mut proposals)| {
            proposals.sort_by(|a, b| a.name.cmp(&b.name));
            DomainChain { name, proposals }
        })
        .collect();
    domains.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(DigestReport {
        total_nodes,
        total_edges,
        proposal_count: proposals.len(),
        seam_count: seams.len(),
        skill_count: skills.len(),
        domains,
        active_seams,
        active_sessions,
        recent_decisions,
    })
}

// ── Types ─────────────────────────────────────────────────────────────────────

pub struct DigestReport {
    pub total_nodes: usize,
    pub total_edges: usize,
    pub proposal_count: usize,
    pub seam_count: usize,
    pub skill_count: usize,
    pub domains: Vec<DomainChain>,
    pub active_seams: Vec<SeamEntry>,
    pub active_sessions: Vec<SessionEntry>,
    pub recent_decisions: Vec<DecisionEntry>,
}

pub struct DomainChain {
    pub name: String,
    pub proposals: Vec<DomainProposalEntry>,
}

pub struct DomainProposalEntry {
    pub id: String,
    pub name: String,
    pub disposition: String,
    pub status: String,
    pub verification: String,
    pub active_seams: Vec<String>,
}

pub struct SeamEntry {
    pub id: String,
    pub name: String,
    pub status: String,
}

pub struct SessionEntry {
    pub id: String,
    pub agent: String,
    pub seam_id: String,
    pub phase: String,
}

pub struct DecisionEntry {
    pub id: String,
    pub name: String,
    pub at: chrono::DateTime<chrono::Utc>,
}

// ── graph_context_for ─────────────────────────────────────────────────────────

/// Assemble a complete work context for an agent given a proposal or seam ID.
/// Returns everything an agent needs in one call: proposal body, related seams,
/// relevant code signatures, verification state, recent decisions, active
/// sessions, and a PlantUML diagram.
pub fn context_for(engine: &GraphEngine, target_id: &str) -> Result<ContextPackage> {
    // Resolve the target node
    let target = engine
        .get_node(target_id)?
        .ok_or_else(|| anyhow::anyhow!("Node '{}' not found", target_id))?;

    let is_proposal = matches!(target.kind, NodeKind::Proposal);
    let is_seam = matches!(target.kind, NodeKind::Seam);

    // Get proposal body from disk if available
    let proposal_body = if let Some(ref path) = target.file_path {
        std::fs::read_to_string(path).ok()
    } else {
        None
    };

    // Outgoing and incoming edges
    let out_edges = engine.get_edges_from(target_id)?;
    let in_edges = engine.get_edges_to(target_id)?;

    // Related seams (if target is a proposal, find its seams; if seam, find its proposals)
    let mut related_seams: Vec<ContextNode> = Vec::new();
    let mut related_proposals: Vec<ContextNode> = Vec::new();
    let mut implementing_code: Vec<ContextSnippet> = Vec::new();
    let mut blocking: Vec<String> = Vec::new();

    for edge in &out_edges {
        if let Some(node) = engine.get_node(&edge.target_id)? {
            match node.kind {
                NodeKind::Seam => related_seams.push(context_node_from(&node)),
                NodeKind::Proposal => related_proposals.push(context_node_from(&node)),
                NodeKind::Type | NodeKind::Function | NodeKind::Module | NodeKind::Crate => {
                    // Get signatures for implementing code (compact, not full body)
                    let snippets = engine.get_snippets_for_node(&node.id)?;
                    for s in snippets {
                        implementing_code.push(ContextSnippet {
                            node_id: node.id.clone(),
                            name: node.name.clone(),
                            kind: node.kind.as_str().to_string(),
                            signature: s.signature.clone(),
                            file_path: s.file_path.clone(),
                            line_start: s.line_start,
                            line_end: s.line_end,
                        });
                    }
                }
                _ => {}
            }
            if edge.relation == EdgeRelation::Blocks {
                blocking.push(edge.target_id.clone());
            }
        }
    }

    for edge in &in_edges {
        if let Some(node) = engine.get_node(&edge.source_id)? {
            match node.kind {
                NodeKind::Seam if is_proposal => related_seams.push(context_node_from(&node)),
                NodeKind::Proposal if is_seam => related_proposals.push(context_node_from(&node)),
                _ => {}
            }
        }
    }

    // Verification state
    let verification_level = target
        .properties
        .get("verification_level")
        .and_then(|v| v.as_str())
        .unwrap_or("none")
        .to_string();

    // Recent test runs against this target
    let test_edges = in_edges
        .iter()
        .filter(|e| {
            matches!(
                e.relation,
                EdgeRelation::TestedBy | EdgeRelation::SmokedBy | EdgeRelation::UatBy
            )
        })
        .take(5);
    let mut recent_runs: Vec<ContextNode> = Vec::new();
    for edge in test_edges {
        if let Some(run) = engine.get_node(&edge.source_id)? {
            recent_runs.push(context_node_from(&run));
        }
    }

    // Recent decisions about this target
    let decision_edges = in_edges
        .iter()
        .filter(|e| e.relation == EdgeRelation::AppliesTo)
        .take(10);
    let mut decisions: Vec<ContextNode> = Vec::new();
    for edge in decision_edges {
        if let Some(d) = engine.get_node(&edge.source_id)? {
            if d.kind == NodeKind::Decision {
                decisions.push(context_node_from(&d));
            }
        }
    }

    // Active sessions working on this target
    let sessions = engine.query_nodes(Some(NodeKind::Session), None)?;
    let active_sessions: Vec<ContextNode> = sessions
        .iter()
        .filter(|s| {
            let is_active = s
                .properties
                .get("status")
                .and_then(|v| v.as_str())
                .map(|s| s == "active")
                .unwrap_or(false);
            let matches_target = s
                .properties
                .get("seam_id")
                .and_then(|v| v.as_str())
                .map(|id| id == target_id)
                .unwrap_or(false)
                || s.properties
                    .get("proposal_id")
                    .and_then(|v| v.as_str())
                    .map(|id| id == target_id)
                    .unwrap_or(false);
            is_active && matches_target
        })
        .map(context_node_from)
        .collect();

    // Generate a PlantUML diagram for this work area
    let diagram = if is_proposal {
        crate::c4::generate_proposal_architecture(engine, target_id).ok()
    } else if is_seam {
        crate::c4::generate_seam_detail(engine, target_id).ok()
    } else {
        None
    };

    let disposition = target
        .properties
        .get("disposition")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let status = target
        .properties
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let domain = target
        .properties
        .get("domain")
        .and_then(|v| v.as_str())
        .unwrap_or("unassigned")
        .to_string();

    Ok(ContextPackage {
        target_id: target_id.to_string(),
        target_name: target.name.clone(),
        target_kind: target.kind.as_str().to_string(),
        domain,
        status,
        disposition,
        verification_level,
        proposal_body,
        related_seams,
        related_proposals,
        implementing_code,
        blocking,
        recent_runs,
        decisions,
        active_sessions,
        diagram,
    })
}

fn context_node_from(node: &crate::schema::Node) -> ContextNode {
    ContextNode {
        id: node.id.clone(),
        name: node.name.clone(),
        kind: node.kind.as_str().to_string(),
        status: node
            .properties
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        properties: node.properties.clone(),
    }
}

pub struct ContextPackage {
    pub target_id: String,
    pub target_name: String,
    pub target_kind: String,
    pub domain: String,
    pub status: String,
    pub disposition: String,
    pub verification_level: String,
    pub proposal_body: Option<String>,
    pub related_seams: Vec<ContextNode>,
    pub related_proposals: Vec<ContextNode>,
    pub implementing_code: Vec<ContextSnippet>,
    pub blocking: Vec<String>,
    pub recent_runs: Vec<ContextNode>,
    pub decisions: Vec<ContextNode>,
    pub active_sessions: Vec<ContextNode>,
    pub diagram: Option<String>,
}

pub struct ContextNode {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub status: String,
    pub properties: serde_json::Value,
}

pub struct ContextSnippet {
    pub node_id: String,
    pub name: String,
    pub kind: String,
    pub signature: String,
    pub file_path: String,
    pub line_start: u32,
    pub line_end: u32,
}

// ── graph_next_task ───────────────────────────────────────────────────────────

/// Find the highest-priority unclaimed work item. Considers:
/// - Proposals with disposition "accepted for current slice" first
/// - Verification level (lower = more work needed = higher priority)
/// - Blocking edges (blockers first)
/// - Active sessions (avoids conflicts)
pub fn next_task(engine: &GraphEngine) -> Result<Option<NextTaskResult>> {
    let proposals = engine.query_nodes(Some(NodeKind::Proposal), None)?;
    let seams = engine.query_nodes(Some(NodeKind::Seam), None)?;
    let sessions = engine.query_nodes(Some(NodeKind::Session), None)?;

    // Build set of currently-claimed targets
    let claimed: std::collections::HashSet<String> = sessions
        .iter()
        .filter(|s| {
            s.properties
                .get("status")
                .and_then(|v| v.as_str())
                .map(|s| s == "active")
                .unwrap_or(false)
        })
        .filter_map(|s| {
            s.properties
                .get("seam_id")
                .and_then(|v| v.as_str())
                .map(|id| id.to_string())
                .or_else(|| {
                    s.properties
                        .get("proposal_id")
                        .and_then(|v| v.as_str())
                        .map(|id| id.to_string())
                })
        })
        .collect();

    // Score and sort proposals
    let mut candidates: Vec<(i32, String, String, String)> = Vec::new();

    for p in &proposals {
        if claimed.contains(&p.id) {
            continue;
        }

        let disposition = p
            .properties
            .get("disposition")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let verification = p
            .properties
            .get("verification_level")
            .and_then(|v| v.as_str())
            .unwrap_or("none");
        let status = p
            .properties
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        // Skip completed/superseded/deferred
        if matches!(disposition, "implemented" | "superseded" | "deferred") {
            continue;
        }
        if matches!(status, "completed" | "archived") {
            continue;
        }

        let mut score: i32 = 0;

        // Priority modifier (1-5 scale, default 3)
        // Higher priority = higher score multiplier
        let priority = p
            .properties
            .get("priority")
            .and_then(|v| v.as_u64())
            .unwrap_or(3) as i32;
        let priority_multiplier = match priority {
            5 => 25, // Critical
            4 => 15, // High
            3 => 10, // Medium (default)
            2 => 5,  // Low
            1 => 2,  // Minimal
            _ => 10, // Default fallback
        };

        // Disposition priority
        score += match disposition {
            "accepted for current slice" => 100,
            "proposed" => 50,
            _ => 25,
        };

        // Apply priority multiplier to base score
        score = (score * priority_multiplier) / 10;

        // Lower verification = more work needed = higher priority
        score += match verification {
            "none" => 40,
            "code-complete" => 30,
            "test-green" => 20,
            "smoke-green" => 10,
            _ => 0,
        };

        // Check if this proposal blocks others (blockers are higher priority)
        let edges = engine.get_edges_from(&p.id).unwrap_or_default();
        let blocks_count = edges
            .iter()
            .filter(|e| e.relation == EdgeRelation::Blocks)
            .count();
        score += (blocks_count as i32) * 15;

        candidates.push((score, p.id.clone(), p.name.clone(), disposition.to_string()));
    }

    // Also consider unclaimed seams
    for s in &seams {
        if claimed.contains(&s.id) {
            continue;
        }
        let status = s
            .properties
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("open");
        if status == "closed" {
            continue;
        }

        // Priority modifier (1-5 scale, default 3)
        let priority = s
            .properties
            .get("priority")
            .and_then(|v| v.as_u64())
            .unwrap_or(3) as i32;
        let priority_multiplier = match priority {
            5 => 25, // Critical
            4 => 15, // High
            3 => 10, // Medium (default)
            2 => 5,  // Low
            1 => 2,  // Minimal
            _ => 10, // Default fallback
        };

        let edges = engine.get_edges_from(&s.id).unwrap_or_default();
        let blocks_count = edges
            .iter()
            .filter(|e| e.relation == EdgeRelation::Blocks)
            .count();

        let mut score = 60 + (blocks_count as i32) * 15; // Seams are generally high-priority
                                                         // Apply priority multiplier to base score
        score = (score * priority_multiplier) / 10;
        candidates.push((score, s.id.clone(), s.name.clone(), status.to_string()));
    }

    candidates.sort_by(|a, b| b.0.cmp(&a.0));

    Ok(candidates.first().map(|(score, id, name, disposition)| {
        let runner_up = candidates
            .get(1)
            .map(|(_, id, name, _)| format!("{} ({})", id, name));
        NextTaskResult {
            id: id.clone(),
            name: name.clone(),
            score: *score,
            disposition: disposition.clone(),
            reason: format_task_reason(*score),
            runner_up,
            total_candidates: candidates.len(),
        }
    }))
}

fn format_task_reason(score: i32) -> String {
    let mut reasons = Vec::new();
    if score >= 100 {
        reasons.push("accepted for current slice");
    } else if score >= 50 {
        reasons.push("proposed");
    }
    if score % 100 >= 40 {
        reasons.push("no verification yet");
    } else if score % 100 >= 30 {
        reasons.push("needs tests");
    }
    if reasons.is_empty() {
        reasons.push("available work");
    }
    reasons.join(", ")
}

pub struct NextTaskResult {
    pub id: String,
    pub name: String,
    pub score: i32,
    pub disposition: String,
    pub reason: String,
    pub runner_up: Option<String>,
    pub total_candidates: usize,
}

// ── graph_impact ──────────────────────────────────────────────────────────────

/// Analyze the blast radius of a change to a file or node.
/// Walks edges to find all affected proposals, seams, and tests.
pub fn impact_analysis(engine: &GraphEngine, target: &str) -> Result<ImpactReport> {
    let mut affected_proposals: Vec<String> = Vec::new();
    let mut affected_seams: Vec<String> = Vec::new();
    let mut affected_tests: Vec<String> = Vec::new();
    let mut _affected_nodes: Vec<String> = Vec::new();

    // If target looks like a file path, find all nodes at that path
    let seed_nodes: Vec<String> = if target.contains('/') || target.contains('.') {
        let all_nodes = engine.query_nodes(None, None)?;
        all_nodes
            .iter()
            .filter(|n| n.file_path.as_deref() == Some(target))
            .map(|n| n.id.clone())
            .collect()
    } else {
        vec![target.to_string()]
    };

    // BFS walk outward from seed nodes through edges
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut queue: std::collections::VecDeque<(String, usize)> = std::collections::VecDeque::new();

    for seed in &seed_nodes {
        queue.push_back((seed.clone(), 0));
        visited.insert(seed.clone());
    }

    let max_depth = 4;

    while let Some((node_id, depth)) = queue.pop_front() {
        if depth > max_depth {
            continue;
        }

        // Check incoming edges (who references/tests/implements this node?)
        let incoming = engine.get_edges_to(&node_id)?;
        for edge in &incoming {
            if visited.contains(&edge.source_id) {
                continue;
            }
            visited.insert(edge.source_id.clone());

            if let Some(node) = engine.get_node(&edge.source_id)? {
                match node.kind {
                    NodeKind::Proposal => affected_proposals.push(node.id.clone()),
                    NodeKind::Seam => affected_seams.push(node.id.clone()),
                    NodeKind::Test | NodeKind::TestRun => affected_tests.push(node.id.clone()),
                    _ => _affected_nodes.push(node.id.clone()),
                }
                if depth + 1 <= max_depth {
                    queue.push_back((edge.source_id.clone(), depth + 1));
                }
            }
        }

        // Check outgoing edges too (what does this node depend on / implement?)
        let outgoing = engine.get_edges_from(&node_id)?;
        for edge in &outgoing {
            if visited.contains(&edge.target_id) {
                continue;
            }
            visited.insert(edge.target_id.clone());

            if let Some(node) = engine.get_node(&edge.target_id)? {
                match node.kind {
                    NodeKind::Proposal => affected_proposals.push(node.id.clone()),
                    NodeKind::Seam => affected_seams.push(node.id.clone()),
                    NodeKind::Test | NodeKind::TestRun => affected_tests.push(node.id.clone()),
                    _ => {}
                }
                if depth + 1 <= max_depth {
                    queue.push_back((edge.target_id.clone(), depth + 1));
                }
            }
        }
    }

    // Deduplicate
    affected_proposals.sort();
    affected_proposals.dedup();
    affected_seams.sort();
    affected_seams.dedup();
    affected_tests.sort();
    affected_tests.dedup();

    Ok(ImpactReport {
        target: target.to_string(),
        seed_nodes: seed_nodes.len(),
        affected_proposals,
        affected_seams,
        affected_tests,
        total_reached: visited.len(),
    })
}

pub struct ImpactReport {
    pub target: String,
    pub seed_nodes: usize,
    pub affected_proposals: Vec<String>,
    pub affected_seams: Vec<String>,
    pub affected_tests: Vec<String>,
    pub total_reached: usize,
}

// ── graph_agent_dashboard ─────────────────────────────────────────────────────

/// Generate a dashboard of all agent activity: active sessions, recent work,
/// per-agent summaries, and verification progress.
pub fn agent_dashboard(engine: &GraphEngine) -> Result<DashboardReport> {
    let sessions = engine.query_nodes(Some(NodeKind::Session), None)?;
    let decisions = engine.query_nodes(Some(NodeKind::Decision), None)?;
    let proposals = engine.query_nodes(Some(NodeKind::Proposal), None)?;

    // Active sessions
    let active_sessions: Vec<DashboardSession> = sessions
        .iter()
        .filter(|s| {
            s.properties
                .get("status")
                .and_then(|v| v.as_str())
                .map(|s| s == "active")
                .unwrap_or(false)
        })
        .map(|s| DashboardSession {
            id: s.id.clone(),
            agent: s
                .properties
                .get("agent")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            seam_id: s
                .properties
                .get("seam_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            proposal_id: s
                .properties
                .get("proposal_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            phase: s
                .properties
                .get("phase")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            started: s.created_at,
            last_activity: s
                .properties
                .get("last_activity")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            tokens_total: s
                .properties
                .get("tokens_total")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            tokens_input: s
                .properties
                .get("tokens_input")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            tokens_output: s
                .properties
                .get("tokens_output")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            elapsed_ms: s
                .properties
                .get("elapsed_ms")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            harness_id: s
                .properties
                .get("harness_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            workflow_name: s
                .properties
                .get("workflow_name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            branch: s
                .properties
                .get("branch")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            worktree_path: s
                .properties
                .get("worktree_path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            files_touched: s
                .properties
                .get("files_touched")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            lines_changed: s
                .properties
                .get("lines_changed")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            test_runs: s
                .properties
                .get("test_runs")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
        })
        .collect();

    // Recent closed sessions (last 20)
    let mut closed_sessions: Vec<DashboardSession> = sessions
        .iter()
        .filter(|s| {
            s.properties
                .get("status")
                .and_then(|v| v.as_str())
                .map(|s| s != "active")
                .unwrap_or(true)
        })
        .map(|s| DashboardSession {
            id: s.id.clone(),
            agent: s
                .properties
                .get("agent")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            seam_id: s
                .properties
                .get("seam_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            proposal_id: s
                .properties
                .get("proposal_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            phase: s
                .properties
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("closed")
                .to_string(),
            started: s.created_at,
            last_activity: s
                .properties
                .get("last_activity")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            tokens_total: s
                .properties
                .get("tokens_total")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            tokens_input: s
                .properties
                .get("tokens_input")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            tokens_output: s
                .properties
                .get("tokens_output")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            elapsed_ms: s
                .properties
                .get("elapsed_ms")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            harness_id: s
                .properties
                .get("harness_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            workflow_name: s
                .properties
                .get("workflow_name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            branch: s
                .properties
                .get("branch")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            worktree_path: s
                .properties
                .get("worktree_path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            files_touched: s
                .properties
                .get("files_touched")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            lines_changed: s
                .properties
                .get("lines_changed")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            test_runs: s
                .properties
                .get("test_runs")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
        })
        .collect();
    closed_sessions.sort_by(|a, b| b.started.cmp(&a.started));
    closed_sessions.truncate(20);

    // Per-agent summary
    let mut agent_map: HashMap<String, AgentSummary> = HashMap::new();
    for s in &sessions {
        let agent = s
            .properties
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let entry = agent_map
            .entry(agent.clone())
            .or_insert_with(|| AgentSummary {
                agent,
                total_sessions: 0,
                active_sessions: 0,
                decisions_made: 0,
                last_active: s.created_at,
            });
        entry.total_sessions += 1;
        if s.properties.get("status").and_then(|v| v.as_str()) == Some("active") {
            entry.active_sessions += 1;
        }
        if s.created_at > entry.last_active {
            entry.last_active = s.created_at;
        }
    }

    // Count decisions per agent
    for d in &decisions {
        if let Some(agent) = d.properties.get("agent").and_then(|v| v.as_str()) {
            if let Some(entry) = agent_map.get_mut(agent) {
                entry.decisions_made += 1;
            }
        }
    }

    let mut agents: Vec<AgentSummary> = agent_map.into_values().collect();
    agents.sort_by(|a, b| b.last_active.cmp(&a.last_active));

    // Verification progress
    let mut verification_counts: HashMap<String, usize> = HashMap::new();
    for p in &proposals {
        let level = p
            .properties
            .get("verification_level")
            .and_then(|v| v.as_str())
            .unwrap_or("none")
            .to_string();
        *verification_counts.entry(level).or_insert(0) += 1;
    }

    Ok(DashboardReport {
        active_sessions,
        recent_closed: closed_sessions,
        agents,
        verification_progress: verification_counts,
        total_proposals: proposals.len(),
        total_sessions: sessions.len(),
        total_decisions: decisions.len(),
    })
}

pub struct DashboardReport {
    pub active_sessions: Vec<DashboardSession>,
    pub recent_closed: Vec<DashboardSession>,
    pub agents: Vec<AgentSummary>,
    pub verification_progress: HashMap<String, usize>,
    pub total_proposals: usize,
    pub total_sessions: usize,
    pub total_decisions: usize,
}

pub struct DashboardSession {
    pub id: String,
    pub agent: String,
    pub seam_id: String,
    pub proposal_id: Option<String>,
    pub phase: String,
    pub started: chrono::DateTime<chrono::Utc>,
    pub last_activity: Option<String>,
    pub tokens_total: i64,
    pub tokens_input: i64,
    pub tokens_output: i64,
    pub elapsed_ms: i64,
    pub harness_id: Option<String>,
    pub workflow_name: Option<String>,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub files_touched: Vec<String>,
    pub lines_changed: i64,
    pub test_runs: i64,
}

pub struct AgentSummary {
    pub agent: String,
    pub total_sessions: usize,
    pub active_sessions: usize,
    pub decisions_made: usize,
    pub last_active: chrono::DateTime<chrono::Utc>,
}

// ── session_health_check ──────────────────────────────────────────────────────

/// Analyze session hygiene: stale sessions, orphaned workstreams, and
/// agent coordination issues. Returns a structured health report.
pub fn session_health_check(engine: &GraphEngine) -> Result<SessionHealthReport> {
    let sessions = engine.query_nodes(Some(NodeKind::Session), None)?;
    let now = Utc::now();

    let mut active_count = 0usize;
    let mut stale_sessions: Vec<StaleSessionEntry> = Vec::new();
    let mut agents_with_multiple: HashMap<String, Vec<String>> = HashMap::new();

    for s in &sessions {
        let status = s
            .properties
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        if status != "active" {
            continue;
        }
        active_count += 1;

        let agent = s
            .properties
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        agents_with_multiple
            .entry(agent.clone())
            .or_default()
            .push(s.id.clone());

        // Check staleness: active sessions with no activity for > 4 hours
        let age = now - s.updated_at;
        let last_activity = s
            .properties
            .get("last_activity")
            .and_then(|v| v.as_str())
            .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
            .map(|dt| now - dt.with_timezone(&Utc));

        let effective_age = last_activity.unwrap_or(age);
        let stale_threshold = chrono::Duration::hours(4);

        if effective_age > stale_threshold {
            stale_sessions.push(StaleSessionEntry {
                id: s.id.clone(),
                agent,
                seam_id: s
                    .properties
                    .get("seam_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string(),
                age_hours: effective_age.num_hours(),
                phase: s
                    .properties
                    .get("phase")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string(),
            });
        }
    }

    // Agents with > 2 concurrent sessions = coordination risk
    let overloaded_agents: Vec<OverloadedAgent> = agents_with_multiple
        .iter()
        .filter(|(_, sessions)| sessions.len() > 2)
        .map(|(agent, sessions)| OverloadedAgent {
            agent: agent.clone(),
            session_count: sessions.len(),
            session_ids: sessions.clone(),
        })
        .collect();

    // Check for orphaned workstreams (workstream nodes with no active session)
    let workstreams = engine.query_nodes(Some(NodeKind::Workstream), None)?;
    let active_session_seam_ids: std::collections::HashSet<String> = sessions
        .iter()
        .filter(|s| s.properties.get("status").and_then(|v| v.as_str()) == Some("active"))
        .filter_map(|s| {
            s.properties
                .get("seam_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect();

    let orphaned_workstreams: Vec<String> = workstreams
        .iter()
        .filter(|w| {
            let ws_status = w
                .properties
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let ws_seam = w
                .properties
                .get("seam_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            ws_status == "active" && !active_session_seam_ids.contains(ws_seam)
        })
        .map(|w| w.id.clone())
        .collect();

    let healthy = stale_sessions.is_empty()
        && overloaded_agents.is_empty()
        && orphaned_workstreams.is_empty();

    Ok(SessionHealthReport {
        healthy,
        active_sessions: active_count,
        total_sessions: sessions.len(),
        stale_sessions,
        overloaded_agents,
        orphaned_workstreams,
    })
}

pub struct SessionHealthReport {
    pub healthy: bool,
    pub active_sessions: usize,
    pub total_sessions: usize,
    pub stale_sessions: Vec<StaleSessionEntry>,
    pub overloaded_agents: Vec<OverloadedAgent>,
    pub orphaned_workstreams: Vec<String>,
}

pub struct StaleSessionEntry {
    pub id: String,
    pub agent: String,
    pub seam_id: String,
    pub age_hours: i64,
    pub phase: String,
}

pub struct OverloadedAgent {
    pub agent: String,
    pub session_count: usize,
    pub session_ids: Vec<String>,
}

// ── proposal_health_check ─────────────────────────────────────────────────────

/// Analyze proposal pipeline health: stuck proposals, missing dispositions,
/// verification gaps, and embedding coverage.
pub fn proposal_health_check(engine: &GraphEngine) -> Result<ProposalHealthReport> {
    let proposals = engine.query_nodes(Some(NodeKind::Proposal), None)?;
    let domain_ids: HashSet<String> = engine
        .query_nodes(Some(NodeKind::Domain), None)?
        .into_iter()
        .map(|domain| domain.id)
        .collect();

    let mut missing_disposition = Vec::new();
    let mut missing_domain = Vec::new();
    let mut missing_domain_node = Vec::new();
    let mut no_verification = Vec::new();
    let mut no_seams = Vec::new();
    let mut no_embedding = Vec::new();
    let mut disposition_counts: HashMap<String, usize> = HashMap::new();
    let mut verification_counts: HashMap<String, usize> = HashMap::new();

    for p in &proposals {
        let disposition = p
            .properties
            .get("disposition")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let domain = p
            .properties
            .get("domain")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let verification = p
            .properties
            .get("verification_level")
            .and_then(|v| v.as_str())
            .unwrap_or("none");

        // Track disposition distribution
        let disp_key = if disposition.is_empty() {
            "missing"
        } else {
            disposition
        };
        *disposition_counts.entry(disp_key.to_string()).or_insert(0) += 1;
        *verification_counts
            .entry(verification.to_string())
            .or_insert(0) += 1;

        if disposition.is_empty() {
            missing_disposition.push(p.id.clone());
        }
        if domain.is_empty() {
            missing_domain.push(p.id.clone());
        } else if !domain_ids.contains(&format!("domain:{}", domain)) {
            missing_domain_node.push(p.id.clone());
        }
        if verification == "none" {
            no_verification.push(p.id.clone());
        }
        if p.embedding.is_none() {
            no_embedding.push(p.id.clone());
        }

        // Check for proposals with no linked seams
        let edges = engine.get_edges_from(&p.id)?;
        let has_seam = edges.iter().any(|e| {
            e.target_id.starts_with("seam:")
                && matches!(
                    e.relation,
                    EdgeRelation::Implements | EdgeRelation::References | EdgeRelation::Governs
                )
        });
        if !has_seam {
            no_seams.push(p.id.clone());
        }
    }

    let healthy = missing_disposition.is_empty()
        && missing_domain_node.is_empty()
        && no_verification.len() < proposals.len() / 2;

    Ok(ProposalHealthReport {
        healthy,
        total_proposals: proposals.len(),
        missing_disposition,
        missing_domain,
        missing_domain_node,
        no_verification,
        no_seams,
        no_embedding,
        disposition_counts,
        verification_counts,
    })
}

pub struct ProposalHealthReport {
    pub healthy: bool,
    pub total_proposals: usize,
    pub missing_disposition: Vec<String>,
    pub missing_domain: Vec<String>,
    pub missing_domain_node: Vec<String>,
    pub no_verification: Vec<String>,
    pub no_seams: Vec<String>,
    pub no_embedding: Vec<String>,
    pub disposition_counts: HashMap<String, usize>,
    pub verification_counts: HashMap<String, usize>,
}

// ── auto_persist_diagrams ─────────────────────────────────────────────────────

/// Auto-generate and persist canonical PlantUML diagrams to
/// `docs/architecture/generated/` after a scan. Writes:
/// - C4 container diagram for the system
/// - Proposal architecture diagram per active proposal
/// - Active seam detail diagrams
pub fn auto_persist_diagrams(engine: &GraphEngine, repo_root: &Path) -> Result<Vec<String>> {
    let generated_dir = repo_root.join("docs/architecture/generated");
    std::fs::create_dir_all(&generated_dir)?;

    let mut written: Vec<String> = Vec::new();

    // 1. C4 Container diagram
    let system_name = repo_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "PhiloticStack".to_string());

    if let Ok(uml) = crate::c4::generate_c4_container(engine, &system_name) {
        let path = generated_dir.join("c4-container.puml");
        std::fs::write(&path, &uml)?;
        written.push(path.display().to_string());
    }

    // 2. Per-proposal architecture diagrams
    let proposals = engine.query_nodes(Some(NodeKind::Proposal), None)?;
    for p in &proposals {
        let disposition = p
            .properties
            .get("disposition")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        // Only generate for active proposals
        if matches!(disposition, "implemented" | "superseded" | "deferred") {
            continue;
        }
        let slug = p.id.replace("doc:", "").replace("proposal:", "");
        if let Ok(uml) = crate::c4::generate_proposal_architecture(engine, &p.id) {
            let path = generated_dir.join(format!("proposal-{}.puml", slug));
            std::fs::write(&path, &uml)?;
            written.push(path.display().to_string());
        }
    }

    // 3. Active seam detail diagrams
    let seams = engine.query_nodes(Some(NodeKind::Seam), None)?;
    for s in &seams {
        let status = s
            .properties
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("open");
        if status == "closed" {
            continue;
        }
        let slug = s.id.replace("seam:", "");
        if let Ok(uml) = crate::c4::generate_seam_detail(engine, &s.id) {
            let path = generated_dir.join(format!("seam-{}.puml", slug));
            std::fs::write(&path, &uml)?;
            written.push(path.display().to_string());
        }
    }

    // 4. Write a README for the generated directory
    let readme = format!(
        "# Generated Architecture Diagrams\n\n\
         > Auto-generated by `graph_scan`. Do not hand-edit.\n\
         > Re-run `graph_scan` or `graph_persist_diagrams` to regenerate.\n\n\
         Generated: {}\n\n\
         ## Files\n\n{}\n",
        Utc::now().format("%Y-%m-%d %H:%M UTC"),
        written
            .iter()
            .map(|p| {
                let name = Path::new(p)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();
                format!("- `{}`", name)
            })
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let readme_path = generated_dir.join("README.md");
    std::fs::write(&readme_path, &readme)?;

    Ok(written)
}
