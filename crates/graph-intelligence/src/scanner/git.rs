use std::path::Path;
use std::process::Command;

use anyhow::Result;
use chrono::Utc;

use crate::engine::GraphEngine;
use crate::schema::*;

/// Scan git metadata from the repository at `repo_root`.
pub fn scan_git(repo_root: &Path, engine: &GraphEngine) -> Result<()> {
    let now = Utc::now();

    // Clear stale branch/worktree nodes before rescan so pruned checkouts
    // don't linger as ghost nodes.
    engine.clear_git_topology_nodes()?;

    // 1. Recent commits
    scan_commits(repo_root, engine, &now)?;

    // 2. Remote branches (written first so worktree scan can look up ahead counts)
    scan_branches(repo_root, engine, &now)?;

    // 3. Worktrees — active checkouts get full enrichment; codex/* branches
    //    without an active checkout are written as pruned stubs.
    scan_worktrees(repo_root, engine, &now)?;

    Ok(())
}

fn scan_commits(
    repo_root: &Path,
    engine: &GraphEngine,
    now: &chrono::DateTime<Utc>,
) -> Result<()> {
    let output = Command::new("git")
        .args(["log", "--oneline", "-100", "--format=%H %s"])
        .current_dir(repo_root)
        .output()?;

    if !output.status.success() {
        eprintln!("Warning: git log failed");
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (sha, message) = match line.split_once(' ') {
            Some((s, m)) => (s.to_string(), m.to_string()),
            None => (line.to_string(), String::new()),
        };

        let short_sha = &sha[..sha.len().min(12)];
        let node_id = format!("commit:{}", short_sha);

        engine.upsert_node(&Node {
            id: node_id,
            kind: NodeKind::Commit,
            name: message,
            properties: serde_json::json!({
                "sha": sha,
            }),
            file_path: None,
            worktree: String::new(),
            created_at: *now,
            updated_at: *now,
            embedding: None,
            embedding_model: None,
            embedding_dims: None,
            embedding_updated: None,
            embedding_hash: None,
        })?;
    }

    // For each recent commit, find which files it touched and create References edges
    let file_output = Command::new("git")
        .args(["log", "--oneline", "-30", "--name-only", "--format=%H"])
        .current_dir(repo_root)
        .output()?;

    if file_output.status.success() {
        let stdout = String::from_utf8_lossy(&file_output.stdout);
        let mut current_sha = String::new();
        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // SHA lines are 40 hex chars
            if line.len() == 40 && line.chars().all(|c| c.is_ascii_hexdigit()) {
                current_sha = line[..12].to_string();
            } else if !current_sha.is_empty() && line.ends_with(".rs") {
                // This is a file path — try to map it to a module
                let commit_id = format!("commit:{}", current_sha);
                // Create a reference to the file path (best effort module mapping)
                let module_id = rs_file_to_module_id(line);
                if !module_id.is_empty() {
                    engine.upsert_edge(&Edge {
                        source_id: commit_id,
                        target_id: module_id,
                        relation: EdgeRelation::References,
                        properties: serde_json::json!({}),
                        worktree: String::new(),
                    }).ok();
                }
            }
        }
    }

    Ok(())
}

fn scan_branches(
    repo_root: &Path,
    engine: &GraphEngine,
    now: &chrono::DateTime<Utc>,
) -> Result<()> {
    let output = Command::new("git")
        .args(["branch", "-r", "--format=%(refname:short)"])
        .current_dir(repo_root)
        .output()?;

    if !output.status.success() {
        eprintln!("Warning: git branch -r failed");
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let branch = line.trim();
        if branch.is_empty() || branch.contains("HEAD") {
            continue;
        }

        let branch_name = branch
            .strip_prefix("origin/")
            .unwrap_or(branch);
        let node_id = format!("branch:{}", branch_name);

        // Get ahead/behind count relative to develop
        let mut ahead = 0u32;
        let mut behind = 0u32;
        if branch_name != "develop" && branch_name != "main" {
            let ab_output = Command::new("git")
                .args([
                    "rev-list",
                    "--left-right",
                    "--count",
                    &format!("origin/develop...{}", branch),
                ])
                .current_dir(repo_root)
                .output();
            if let Ok(ab) = ab_output {
                if ab.status.success() {
                    let ab_str = String::from_utf8_lossy(&ab.stdout);
                    let parts: Vec<&str> = ab_str.trim().split('\t').collect();
                    if parts.len() == 2 {
                        behind = parts[0].parse().unwrap_or(0);
                        ahead = parts[1].parse().unwrap_or(0);
                    }
                }
            }
        }

        engine.upsert_node(&Node {
            id: node_id,
            kind: NodeKind::Branch,
            name: branch_name.to_string(),
            properties: serde_json::json!({
                "remote": branch,
                "ahead": ahead,
                "behind": behind,
            }),
            file_path: None,
            worktree: String::new(),
            created_at: *now,
            updated_at: *now,
            embedding: None,
            embedding_model: None,
            embedding_dims: None,
            embedding_updated: None,
            embedding_hash: None,
        })?;
    }

    Ok(())
}

fn scan_worktrees(
    repo_root: &Path,
    engine: &GraphEngine,
    now: &chrono::DateTime<Utc>,
) -> Result<()> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo_root)
        .output()?;

    if !output.status.success() {
        return Ok(());
    }

    // Pre-load all proposal nodes so we can do slug matching without repeated queries.
    let proposals = engine.query_nodes(Some(NodeKind::Proposal), None).unwrap_or_default();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut current_path = String::new();
    let mut current_branch = String::new();
    let mut active_branches: std::collections::HashSet<String> = std::collections::HashSet::new();

    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = path.to_string();
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            current_branch = branch.to_string();
        } else if line.is_empty() && !current_path.is_empty() {
            if !current_branch.is_empty() {
                active_branches.insert(current_branch.clone());
            }
            upsert_worktree(engine, now, &proposals, &current_path, &current_branch, repo_root)?;
            current_path.clear();
            current_branch.clear();
        }
    }

    // Flush last entry if no trailing blank line
    if !current_path.is_empty() {
        if !current_branch.is_empty() {
            active_branches.insert(current_branch.clone());
        }
        upsert_worktree(engine, now, &proposals, &current_path, &current_branch, repo_root)?;
    }

    // Write pruned stubs for codex/* branches that exist but have no active checkout.
    // This lets the graph distinguish "branch exists, worktree removed" from "branch gone".
    let all_branches = list_local_branches(repo_root);
    for branch in all_branches {
        if !branch.starts_with("codex/") {
            continue;
        }
        if active_branches.contains(&branch) {
            continue;
        }
        let wt_id = format!("worktree:{}", branch);
        let slug = branch.strip_prefix("codex/").unwrap_or(&branch);
        let linked = proposals.iter().find(|p| {
            p.id.strip_prefix("doc:")
                .map(|pid| pid == slug || pid.starts_with(slug))
                .unwrap_or(false)
        });
        let linked_proposal_id = linked.map(|p| p.id.clone());
        let proposal_status = linked
            .and_then(|p| p.properties.get("status"))
            .and_then(|s| s.as_str())
            .unwrap_or("");
        let ahead = commit_count_ahead(repo_root, &branch);
        let phase = infer_workstream_phase(proposal_status, ahead);

        engine.upsert_node(&Node {
            id: wt_id.clone(),
            kind: NodeKind::Worktree,
            name: branch.clone(),
            properties: serde_json::json!({
                "branch": branch,
                "phase": phase,
                "pruned": true,
                "linked_proposal": linked_proposal_id,
            }),
            file_path: None,
            worktree: String::new(),
            created_at: *now,
            updated_at: *now,
            embedding: None,
            embedding_model: None,
            embedding_dims: None,
            embedding_updated: None,
            embedding_hash: None,
        })?;

        if let Some(ref prop_id) = linked_proposal_id {
            engine.upsert_edge(&Edge {
                source_id: prop_id.clone(),
                target_id: wt_id,
                relation: EdgeRelation::Drives,
                properties: serde_json::json!({"phase": phase, "pruned": true}),
                worktree: String::new(),
            })?;
        }
    }

    Ok(())
}

/// List all local branches via `git branch`.
fn list_local_branches(repo_root: &Path) -> Vec<String> {
    let out = Command::new("git")
        .args(["branch", "--format=%(refname:short)"])
        .current_dir(repo_root)
        .output();
    out.ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

/// Upsert a single worktree node, infer its workstream phase, and link it to a proposal if found.
fn upsert_worktree(
    engine: &GraphEngine,
    now: &chrono::DateTime<Utc>,
    proposals: &[Node],
    path: &str,
    branch: &str,
    repo_root: &Path,
) -> Result<()> {
    let name = if branch.is_empty() { path } else { branch };
    let wt_id = format!("worktree:{}", name);

    // For codex/* branches: extract the slug, find a linked proposal, infer phase.
    let (linked_proposal_id, phase) = if let Some(slug) = branch.strip_prefix("codex/") {
        let matched = proposals.iter().find(|p| {
            // Match by proposal_id field embedded in the node id: "doc:{proposal_id}"
            p.id.strip_prefix("doc:")
                .map(|pid| pid == slug || pid.starts_with(slug))
                .unwrap_or(false)
        });

        let proposal_id = matched.map(|p| p.id.clone());

        // Infer phase from proposal status + ahead commit count.
        let proposal_status = matched
            .and_then(|p| p.properties.get("status"))
            .and_then(|s| s.as_str())
            .unwrap_or("");

        // Count commits ahead of develop for this branch.
        let ahead = commit_count_ahead(repo_root, branch);

        let phase = infer_workstream_phase(proposal_status, ahead);

        (proposal_id, phase)
    } else {
        (None, "active".to_string())
    };

    engine.upsert_node(&Node {
        id: wt_id.clone(),
        kind: NodeKind::Worktree,
        name: name.to_string(),
        properties: serde_json::json!({
            "path": path,
            "branch": branch,
            "phase": phase,
            "linked_proposal": linked_proposal_id,
        }),
        file_path: None,
        worktree: String::new(),
        created_at: *now,
        updated_at: *now,
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    })?;

    // Write Drives edge: proposal → worktree (if linked).
    if let Some(ref prop_id) = linked_proposal_id {
        engine.upsert_edge(&Edge {
            source_id: prop_id.clone(),
            target_id: wt_id,
            relation: EdgeRelation::Drives,
            properties: serde_json::json!({"phase": phase}),
            worktree: String::new(),
        })?;
    }

    Ok(())
}

/// Count commits on `branch` that are ahead of develop.
fn commit_count_ahead(repo_root: &Path, branch: &str) -> u32 {
    let out = Command::new("git")
        .args(["rev-list", "--count", &format!("origin/develop..{}", branch)])
        .current_dir(repo_root)
        .output();
    out.ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0)
}

/// Derive a workstream phase from proposal status and commit count.
fn infer_workstream_phase(proposal_status: &str, ahead: u32) -> String {
    match proposal_status {
        s if s.starts_with("accepted") && ahead > 0 => "implementation",
        s if s.starts_with("accepted") => "planning",
        "proposed" if ahead == 0 => "planning",
        "proposed" => "implementation",
        "in-progress" => "implementation",
        "process-documentation" => "implementation",
        _ if ahead > 0 => "implementation",
        _ => "planning",
    }
    .to_string()
}

/// Convert a .rs file path like `crates/aiua/src/service/ipc.rs` to a module ID.
fn rs_file_to_module_id(path: &str) -> String {
    // e.g., crates/aiua/src/service/ipc.rs -> module:aiua::service::ipc
    let path = path.trim_end_matches(".rs");
    let parts: Vec<&str> = path.split('/').collect();

    // Look for "src" in the path
    if let Some(src_idx) = parts.iter().position(|&p| p == "src") {
        let crate_name = if src_idx > 0 {
            parts[src_idx - 1]
        } else {
            return String::new();
        };

        let mod_parts: Vec<&str> = parts[src_idx + 1..].to_vec();
        if mod_parts.is_empty() {
            return format!("module:{}", crate_name);
        }

        let mod_path = mod_parts.join("::");
        // Handle lib/main/mod
        if mod_path == "lib" || mod_path == "main" {
            format!("module:{}", crate_name)
        } else if mod_path.ends_with("::mod") {
            format!("module:{}::{}", crate_name, &mod_path[..mod_path.len() - 5])
        } else {
            format!("module:{}::{}", crate_name, mod_path)
        }
    } else {
        String::new()
    }
}
