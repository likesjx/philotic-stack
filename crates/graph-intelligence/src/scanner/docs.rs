use std::fs;
use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use regex::Regex;
use serde::Deserialize;
use walkdir::WalkDir;

use crate::engine::GraphEngine;
use crate::schema::*;

/// Frontmatter fields we try to extract from markdown docs.
#[derive(Debug, Default, Deserialize)]
struct DocFrontmatter {
    title: Option<String>,
    doc_type: Option<String>,
    domain: Option<String>,
    status: Option<String>,
    disposition: Option<String>,
    proposal_id: Option<String>,
    related_docs: Option<Vec<String>>,
    task_refs: Option<Vec<String>>,
    implements: Option<Vec<String>>,
    implemented_by: Option<Vec<String>>,
    active_seams: Option<Vec<String>>,
    source_of_truth_targets: Option<Vec<String>>,
    tags: Option<Vec<String>>,
    last_updated: Option<String>,
    sver: Option<String>,
}

/// Scan all `.md` files under docs/, skills/, workflows/, and root.
/// Returns the number of document nodes created.
pub fn scan_docs(root: &Path, engine: &GraphEngine) -> Result<usize> {
    let now = Utc::now();
    let mut count = 0;

    let existing_doc_nodes = existing_doc_node_map(engine)?;

    // Clear stale doc nodes before rescan (prevents ghosts from renamed/deleted files)
    engine.clear_scanned_doc_nodes()?;

    // Collect all .md files from multiple scan roots
    let scan_dirs = ["docs", "skills", "workflows"];
    let mut md_files: Vec<std::path::PathBuf> = Vec::new();

    for dir_name in &scan_dirs {
        let dir = root.join(dir_name);
        if dir.exists() {
            for entry in WalkDir::new(&dir)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map_or(false, |ext| ext == "md"))
            {
                md_files.push(entry.path().to_path_buf());
            }
        }
    }

    // Also scan root-level .md files (AGENTS.md, CLAUDE.md, README.md)
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "md") && path.is_file() {
                md_files.push(path);
            }
        }
    }

    // Prime the graph with authoritative domain nodes before linking proposals/seams.
    if let Some(domain_map_path) = md_files.iter().find(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(|name| name == "DOMAIN_MAP.md")
            .unwrap_or(false)
    }) {
        if let Ok(content) = fs::read_to_string(domain_map_path) {
            let (_, body) = parse_frontmatter(&content);
            parse_domain_map(&body, engine)?;
        }
    }

    for file_path in &md_files {
        let rel_path = file_path
            .strip_prefix(root)
            .unwrap_or(file_path)
            .display()
            .to_string();

        let content = match fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let (frontmatter, body) = parse_frontmatter(&content);

        let file_name = doc_identity_stem(file_path);

        let title = frontmatter
            .title
            .clone()
            .unwrap_or_else(|| file_name.clone());

        let is_skill = rel_path.starts_with("skills/");
        let is_workflow = rel_path.starts_with("workflows/");

        let node_kind = match frontmatter.doc_type.as_deref() {
            Some("proposal") => NodeKind::Proposal,
            Some("reference") | Some("status") | Some("historical") | Some("architecture") => {
                NodeKind::Document
            }
            Some("workflow") => NodeKind::Document,
            Some("seam") => NodeKind::Seam,
            Some("task-surface") | Some("task") => NodeKind::Task,
            Some("sver") => NodeKind::Sver,
            Some("skill") => NodeKind::Skill,
            _ => {
                // Infer from path and filename
                if is_skill {
                    NodeKind::Skill
                } else if is_workflow {
                    NodeKind::Document
                } else if rel_path.contains("PROPOSAL") {
                    NodeKind::Proposal
                } else if rel_path.contains("task") {
                    NodeKind::Task
                } else {
                    NodeKind::Document
                }
            }
        };

        let id_prefix = match node_kind {
            NodeKind::Skill => "skill",
            NodeKind::Sver => "sver",
            NodeKind::Proposal => "doc",
            NodeKind::Seam => "seam",
            NodeKind::Task => "task",
            NodeKind::Document => "doc",
            _ => "doc",
        };
        let node_id = format!(
            "{}:{}",
            id_prefix,
            frontmatter
                .proposal_id
                .clone()
                .unwrap_or_else(|| slugify(&file_name))
        );

        let scanned_properties = serde_json::json!({
            "status": frontmatter.status,
            "disposition": frontmatter
                .disposition
                .clone()
                .or_else(|| normalize_disposition(frontmatter.status.as_deref())),
            "domain": frontmatter.domain,
            "doc_type": frontmatter.doc_type,
            "tags": frontmatter.tags,
            "source_of_truth_targets": frontmatter.source_of_truth_targets,
            "last_updated": frontmatter.last_updated,
            "content": content,
            "content_format": "markdown",
            "content_source": "scan:file",
        });

        let properties = merge_doc_properties(existing_doc_nodes.get(&node_id), scanned_properties);

        engine.upsert_node(&Node {
            id: node_id.clone(),
            kind: node_kind,
            name: title.clone(),
            properties,
            file_path: Some(rel_path.clone()),
            worktree: String::new(),
            created_at: now,
            embedding: None,
            embedding_model: None,
            embedding_dims: None,
            embedding_updated: None,
            embedding_hash: None,
            updated_at: now,
        })?;
        count += 1;

        if let Some(domain) = frontmatter.domain.as_deref() {
            let domain_id = format!("domain:{}", domain);
            if engine.get_node(&domain_id)?.is_some() {
                engine.upsert_edge(&Edge {
                    source_id: domain_id,
                    target_id: node_id.clone(),
                    relation: EdgeRelation::Governs,
                    properties: serde_json::json!({"source": "proposal_frontmatter"}),
                    worktree: String::new(),
                })?;
            } else {
                eprintln!(
                    "[scan] Proposal {} references unknown domain '{}'. Add it to DOMAIN_MAP.md before treating it as canonical.",
                    node_id, domain
                );
            }
        }

        // Create edges for related_docs
        if let Some(refs) = &frontmatter.related_docs {
            for related in refs {
                let target_id = format!("doc:{}", slugify(&related.replace(".md", "")));
                engine.upsert_edge(&Edge {
                    source_id: node_id.clone(),
                    target_id,
                    relation: EdgeRelation::References,
                    properties: serde_json::json!({}),
                    worktree: String::new(),
                })?;
            }
        }

        // Create edges for implements
        if let Some(impls) = &frontmatter.implements {
            for imp in impls {
                let target_id = format!("seam:{}", imp);
                engine.upsert_edge(&Edge {
                    source_id: node_id.clone(),
                    target_id,
                    relation: EdgeRelation::Implements,
                    properties: serde_json::json!({}),
                    worktree: String::new(),
                })?;
            }
        }

        // Create edges for implemented_by (slices)
        if let Some(slices) = &frontmatter.implemented_by {
            for slice in slices {
                let slice_id = format!("slice:{}", slice);
                engine.upsert_node(&Node {
                    id: slice_id.clone(),
                    kind: NodeKind::Slice,
                    name: slice.clone(),
                    properties: serde_json::json!({}),
                    file_path: None,
                    worktree: String::new(),
                    created_at: now,
                    embedding: None,
                    embedding_model: None,
                    embedding_dims: None,
                    embedding_updated: None,
                    embedding_hash: None,
                    updated_at: now,
                })?;
                engine.upsert_edge(&Edge {
                    source_id: node_id.clone(),
                    target_id: slice_id,
                    relation: EdgeRelation::ImplementedBy,
                    properties: serde_json::json!({}),
                    worktree: String::new(),
                })?;
            }
        }

        // Create edges for active_seams
        if let Some(seams) = &frontmatter.active_seams {
            for seam in seams {
                let seam_id = format!("seam:{}", seam);
                // Ensure the seam node exists
                engine.upsert_node(&Node {
                    id: seam_id.clone(),
                    kind: NodeKind::Seam,
                    name: seam.clone(),
                    properties: serde_json::json!({}),
                    file_path: None,
                    worktree: String::new(),
                    created_at: now,
                    embedding: None,
                    embedding_model: None,
                    embedding_dims: None,
                    embedding_updated: None,
                    embedding_hash: None,
                    updated_at: now,
                })?;
                engine.upsert_edge(&Edge {
                    source_id: node_id.clone(),
                    target_id: seam_id,
                    relation: EdgeRelation::AppliesTo,
                    properties: serde_json::json!({}),
                    worktree: String::new(),
                })?;
            }
        }

        // Create edges for task_refs
        if let Some(tasks) = &frontmatter.task_refs {
            for task_ref in tasks {
                let task_id = format!("task:{}", slugify(&task_ref.replace(".md", "")));
                engine.upsert_edge(&Edge {
                    source_id: node_id.clone(),
                    target_id: task_id,
                    relation: EdgeRelation::References,
                    properties: serde_json::json!({}),
                    worktree: String::new(),
                })?;
            }
        }

        // Create UsesSver edge if sver field is present
        if let Some(sver_ref) = &frontmatter.sver {
            let sver_target = format!("doc:{}", slugify(sver_ref));
            engine.upsert_edge(&Edge {
                source_id: node_id.clone(),
                target_id: sver_target,
                relation: EdgeRelation::UsesSver,
                properties: serde_json::json!({}),
                worktree: String::new(),
            })?;
        }

        // If this is the SEAM_REGISTRY, parse seam table rows
        if file_name == "SEAM_REGISTRY" {
            parse_seam_registry(&body, engine)?;
        }

        // If this is task.md, parse task items
        if file_name == "task" {
            parse_task_items(&body, engine)?;
        }
    }

    Ok(count)
}

fn existing_doc_node_map(engine: &GraphEngine) -> Result<std::collections::HashMap<String, Node>> {
    let mut map = std::collections::HashMap::new();
    for kind in [
        NodeKind::Proposal,
        NodeKind::Domain,
        NodeKind::Seam,
        NodeKind::Task,
        NodeKind::Document,
        NodeKind::Skill,
        NodeKind::Sver,
    ] {
        for node in engine.query_nodes(Some(kind), None)? {
            if node.worktree.is_empty() {
                map.insert(node.id.clone(), node);
            }
        }
    }
    Ok(map)
}

fn merge_doc_properties(existing: Option<&Node>, scanned: serde_json::Value) -> serde_json::Value {
    let mut merged = scanned.as_object().cloned().unwrap_or_default();

    if let Some(existing) = existing {
        if let Some(existing_props) = existing.properties.as_object() {
            for (key, value) in existing_props {
                if key == "content" || key == "content_format" || key == "content_source" {
                    let is_graph_authored_content = existing_props
                        .get("content_source")
                        .and_then(|v| v.as_str())
                        == Some("graph");
                    if is_graph_authored_content {
                        merged.insert(key.clone(), value.clone());
                    }
                    continue;
                }

                let scanned_missing = merged.get(key).map(is_missing_value).unwrap_or(true);
                if scanned_missing {
                    merged.insert(key.clone(), value.clone());
                }
            }
        }
    }

    serde_json::Value::Object(merged)
}

fn is_missing_value(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::String(s) => s.is_empty(),
        serde_json::Value::Array(items) => items.is_empty(),
        serde_json::Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

/// Parse YAML frontmatter delimited by `---` lines.
fn parse_frontmatter(content: &str) -> (DocFrontmatter, String) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (DocFrontmatter::default(), content.to_string());
    }

    // Find the closing `---`
    let after_first = &trimmed[3..];
    if let Some(end) = after_first.find("\n---") {
        let yaml_str = &after_first[..end];
        let body = &after_first[end + 4..];

        let fm: DocFrontmatter = serde_yaml::from_str(yaml_str).unwrap_or_default();
        (fm, body.to_string())
    } else {
        (DocFrontmatter::default(), content.to_string())
    }
}

/// Parse the seam registry markdown table to create Seam nodes.
fn parse_seam_registry(body: &str, engine: &GraphEngine) -> Result<()> {
    let now = Utc::now();

    // Regex to extract [Name.md](path) or just Name.md
    let proposal_ref_regex = Regex::new(r"\[([^\]]+\.md)\]\([^)]*\)|([^\s]+\.md)").unwrap();

    for line in body.lines() {
        let line = line.trim();
        if !line.starts_with('|') || line.starts_with("| ---") || line.starts_with("| Seam") {
            continue;
        }

        let cols: Vec<&str> = line.split('|').map(|s| s.trim()).collect();
        // Expected: ["", seam_id, domain, proposal, task_surface, ""]
        if cols.len() >= 5 {
            let seam_id = cols[1].trim_matches('`').trim();
            let domain = cols[2].trim();
            let proposal_ref = cols[3].trim();

            if seam_id.is_empty() || seam_id.contains("---") {
                continue;
            }

            let node_id = format!("seam:{}", seam_id);
            engine.upsert_node(&Node {
                id: node_id.clone(),
                kind: NodeKind::Seam,
                name: seam_id.to_string(),
                properties: serde_json::json!({
                    "domain": domain,
                    "proposal_ref": proposal_ref,
                }),
                file_path: Some("docs/architecture/SEAM_REGISTRY.md".into()),
                worktree: String::new(),
                created_at: now,
                embedding: None,
                embedding_model: None,
                embedding_dims: None,
                embedding_updated: None,
                embedding_hash: None,
                updated_at: now,
            })?;

            // Create edge to proposal if proposal_ref contains a markdown link
            if !proposal_ref.is_empty() {
                // Extract proposal ID from markdown link [Name.md](path) or just Name.md
                let proposal_id = if let Some(caps) = proposal_ref_regex.captures(proposal_ref) {
                    caps.get(1).map(|m| m.as_str()).unwrap_or(proposal_ref)
                } else {
                    proposal_ref
                };
                let proposal_slug = slugify(&proposal_id.replace(".md", ""));
                let proposal_node_id = format!("doc:{}", proposal_slug);
                engine.upsert_edge(&Edge {
                    source_id: node_id.clone(),
                    target_id: proposal_node_id,
                    relation: EdgeRelation::References,
                    properties: serde_json::json!({"ref_type": "proposal"}),
                    worktree: String::new(),
                })?;
            }

            // Link seam to a known domain node if the domain exists in DOMAIN_MAP.
            if !domain.is_empty() {
                let domain_id = format!("domain:{}", domain);
                if engine.get_node(&domain_id)?.is_some() {
                    engine.upsert_edge(&Edge {
                        source_id: domain_id,
                        target_id: node_id.clone(),
                        relation: EdgeRelation::Contains,
                        properties: serde_json::json!({"source": "seam_registry"}),
                        worktree: String::new(),
                    })?;
                } else {
                    eprintln!(
                        "[scan] Seam {} references unknown domain '{}'. Add the domain to DOMAIN_MAP.md first.",
                        seam_id, domain
                    );
                }
            }
        }
    }
    Ok(())
}

/// Parse the authoritative domain catalog from DOMAIN_MAP.md and create first-class domain nodes.
fn parse_domain_map(body: &str, engine: &GraphEngine) -> Result<()> {
    let now = Utc::now();

    let mut current_title = String::new();
    let mut current_domain_id = String::new();
    let mut current_truth: Vec<String> = Vec::new();
    let mut active_proposals: Vec<String> = Vec::new();
    let mut supporting_docs: Vec<String> = Vec::new();
    let mut current_bucket = "";

    let commit_section = |engine: &GraphEngine,
                          title: &str,
                          domain_id: &str,
                          current_truth: &[String],
                          active_proposals: &[String],
                          supporting_docs: &[String]|
     -> Result<()> {
        if domain_id.is_empty() {
            return Ok(());
        }

        let node_id = format!("domain:{}", domain_id);
        engine.upsert_node(&Node {
            id: node_id,
            kind: NodeKind::Domain,
            name: title.to_string(),
            properties: serde_json::json!({
                "scope": title,
                "current_truth": current_truth,
                "active_proposals": active_proposals,
                "supporting_docs": supporting_docs,
                "source": "docs/architecture/DOMAIN_MAP.md",
            }),
            file_path: Some("docs/architecture/DOMAIN_MAP.md".into()),
            worktree: String::new(),
            created_at: now,
            embedding: None,
            embedding_model: None,
            embedding_dims: None,
            embedding_updated: None,
            embedding_hash: None,
            updated_at: now,
        })?;

        Ok(())
    };

    for line in body.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("## ") {
            commit_section(
                engine,
                &current_title,
                &current_domain_id,
                &current_truth,
                &active_proposals,
                &supporting_docs,
            )?;
            current_title = trimmed.trim_start_matches("## ").trim().to_string();
            current_domain_id.clear();
            current_truth.clear();
            active_proposals.clear();
            supporting_docs.clear();
            current_bucket = "";
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("Primary domain id:") {
            current_domain_id = rest.trim().trim_matches('`').to_string();
            continue;
        }

        current_bucket = match trimmed {
            "Current truth:" => "truth",
            "Active proposals:" => "proposals",
            "Supporting docs:" => "supporting",
            _ => current_bucket,
        };

        if trimmed.starts_with("-") {
            let item = trimmed.trim_start_matches('-').trim();
            if item.is_empty() {
                continue;
            }
            match current_bucket {
                "truth" => current_truth.push(item.to_string()),
                "proposals" => active_proposals.push(item.to_string()),
                "supporting" => supporting_docs.push(item.to_string()),
                _ => {}
            }
        }
    }

    commit_section(
        engine,
        &current_title,
        &current_domain_id,
        &current_truth,
        &active_proposals,
        &supporting_docs,
    )?;

    Ok(())
}

/// Parse task.md to create task nodes from bulleted/numbered items.
fn parse_task_items(body: &str, engine: &GraphEngine) -> Result<()> {
    let now = Utc::now();
    let mut current_section = String::new();
    let mut task_idx = 0;

    for line in body.lines() {
        let trimmed = line.trim();

        // Track section headers
        if trimmed.starts_with('#') {
            current_section = trimmed.trim_start_matches('#').trim().to_string();
            continue;
        }

        // Match bulleted or numbered items
        let is_task = trimmed.starts_with("- [")
            || trimmed.starts_with("* [")
            || (trimmed.len() > 2 && trimmed.chars().next().map_or(false, |c| c.is_ascii_digit()));

        if is_task {
            task_idx += 1;
            let done = trimmed.contains("[x]") || trimmed.contains("[X]");
            let task_text = trimmed
                .trim_start_matches(|c: char| {
                    c == '-'
                        || c == '*'
                        || c == ' '
                        || c.is_ascii_digit()
                        || c == '.'
                        || c == '['
                        || c == ']'
                        || c == 'x'
                        || c == 'X'
                })
                .trim();

            let task_id = format!("task:task-md-{}", task_idx);
            engine.upsert_node(&Node {
                id: task_id,
                kind: NodeKind::Task,
                name: task_text.to_string(),
                properties: serde_json::json!({
                    "section": current_section,
                    "done": done,
                }),
                file_path: Some("docs/task.md".into()),
                worktree: String::new(),
                created_at: now,
                embedding: None,
                embedding_model: None,
                embedding_dims: None,
                embedding_updated: None,
                embedding_hash: None,
                updated_at: now,
            })?;
        }
    }
    Ok(())
}

/// Identity stem for a doc file. Container-style filenames
/// (skills/foo/SKILL.md, docs/bar/README.md) carry their identity in the
/// parent directory; using the bare stem collapses every such file onto one
/// node id (e.g. skill:skill), each upsert overwriting the previous one.
fn doc_identity_stem(file_path: &Path) -> String {
    let stem = file_path
        .file_stem()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let parent_name = file_path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string());
    match (stem.to_ascii_lowercase().as_str(), parent_name) {
        ("skill", Some(parent)) => parent,
        ("readme", Some(parent)) => format!("{}-readme", parent),
        _ => stem,
    }
}

fn slugify(s: &str) -> String {
    s.to_lowercase()
        .replace(' ', "-")
        .replace('_', "-")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect()
}

fn normalize_disposition(status: Option<&str>) -> Option<String> {
    let status = status?.trim();

    match status {
        "proposed" | "draft" => Some("proposed".to_string()),
        "accepted-current-slice" | "accepted for current slice" | "accepted_current_slice" => {
            Some("accepted-current-slice".to_string())
        }
        "in-progress" | "in progress" | "active" => Some("accepted-current-slice".to_string()),
        "implemented" => Some("implemented".to_string()),
        "superseded" => Some("superseded".to_string()),
        "deferred" => Some("deferred".to_string()),
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{doc_identity_stem, normalize_disposition, parse_frontmatter};
    use std::path::Path;

    #[test]
    fn container_style_filenames_take_identity_from_parent_dir() {
        assert_eq!(
            doc_identity_stem(Path::new("skills/graph-intelligence/SKILL.md")),
            "graph-intelligence"
        );
        assert_eq!(
            doc_identity_stem(Path::new("skills/check-engine/SKILL.md")),
            "check-engine"
        );
        assert_eq!(
            doc_identity_stem(Path::new("docs/architecture/README.md")),
            "architecture-readme"
        );
        assert_eq!(
            doc_identity_stem(Path::new("docs/architecture/ARCHITECTURE.md")),
            "ARCHITECTURE"
        );
    }

    #[test]
    fn parses_array_tags_and_status_fields() {
        let content = r#"---
title: Example Proposal
doc_type: proposal
domain: runtime-sessions
status: implemented
tags:
  - sessions
  - approvals
related_docs:
  - ARCHITECTURE_STATUS.md
---

# Body
"#;

        let (frontmatter, body) = parse_frontmatter(content);
        assert_eq!(frontmatter.title.as_deref(), Some("Example Proposal"));
        assert_eq!(frontmatter.doc_type.as_deref(), Some("proposal"));
        assert_eq!(frontmatter.domain.as_deref(), Some("runtime-sessions"));
        assert_eq!(frontmatter.status.as_deref(), Some("implemented"));
        assert_eq!(
            frontmatter.tags.as_ref().unwrap(),
            &vec!["sessions".to_string(), "approvals".to_string()]
        );
        assert!(body.contains("# Body"));
    }

    #[test]
    fn normalizes_common_disposition_aliases() {
        assert_eq!(
            normalize_disposition(Some("draft")).as_deref(),
            Some("proposed")
        );
        assert_eq!(
            normalize_disposition(Some("in-progress")).as_deref(),
            Some("accepted-current-slice")
        );
        assert_eq!(
            normalize_disposition(Some("implemented")).as_deref(),
            Some("implemented")
        );
    }
}
