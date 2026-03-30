use std::fs;
use std::path::Path;

use anyhow::Result;
use chrono::Utc;
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
    tags: Option<String>,
    last_updated: Option<String>,
    sver: Option<String>,
}

/// Scan all `.md` files under docs/, skills/, workflows/, and root.
/// Returns the number of document nodes created.
pub fn scan_docs(root: &Path, engine: &GraphEngine) -> Result<usize> {
    let now = Utc::now();
    let mut count = 0;

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

    for file_path in &md_files
    {
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

        let file_name = file_path
            .file_stem()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let title = frontmatter
            .title
            .clone()
            .unwrap_or_else(|| file_name.clone());

        let is_skill = rel_path.starts_with("skills/");
        let is_workflow = rel_path.starts_with("workflows/");

        let node_kind = match frontmatter.doc_type.as_deref() {
            Some("proposal") => NodeKind::Proposal,
            Some("reference") | Some("status") | Some("historical") | Some("architecture") => NodeKind::Domain,
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
                    NodeKind::Domain
                }
            }
        };

        let id_prefix = match node_kind {
            NodeKind::Skill => "skill",
            NodeKind::Sver => "sver",
            NodeKind::Proposal => "doc",
            NodeKind::Seam => "seam",
            NodeKind::Task => "task",
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

        let properties = serde_json::json!({
            "status": frontmatter.status,
            "disposition": frontmatter.disposition,
            "domain": frontmatter.domain,
            "doc_type": frontmatter.doc_type,
            "tags": frontmatter.tags,
            "source_of_truth_targets": frontmatter.source_of_truth_targets,
            "last_updated": frontmatter.last_updated,
        });

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

    for line in body.lines() {
        let line = line.trim();
        if !line.starts_with('|') || line.starts_with("| ---") || line.starts_with("| Seam") {
            continue;
        }

        let cols: Vec<&str> = line.split('|').map(|s| s.trim()).collect();
        // Expected: ["", seam_id, domain, proposal, task_surface, ""]
        if cols.len() >= 5 {
            let seam_id = cols[1]
                .trim_matches('`')
                .trim();
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

            // Create domain node and edge
            if !domain.is_empty() {
                let domain_id = format!("domain:{}", domain);
                engine.upsert_node(&Node {
                    id: domain_id.clone(),
                    kind: NodeKind::Domain,
                    name: domain.to_string(),
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
                    source_id: domain_id,
                    target_id: node_id.clone(),
                    relation: EdgeRelation::Contains,
                    properties: serde_json::json!({}),
                    worktree: String::new(),
                })?;
            }
        }
    }
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
            current_section = trimmed
                .trim_start_matches('#')
                .trim()
                .to_string();
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
                .trim_start_matches(|c: char| c == '-' || c == '*' || c == ' ' || c.is_ascii_digit() || c == '.' || c == '[' || c == ']' || c == 'x' || c == 'X')
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

fn slugify(s: &str) -> String {
    s.to_lowercase()
        .replace(' ', "-")
        .replace('_', "-")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect()
}
