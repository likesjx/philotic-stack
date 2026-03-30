pub mod code;
pub mod docs;
pub mod git;

use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use chrono::Utc;

use crate::engine::GraphEngine;

/// Configuration for a full workspace scan.
pub struct ScanConfig {
    pub rust_roots: Vec<String>,
    pub doc_roots: Vec<String>,
    pub git_repo: String,
    pub worktree: String,
}

/// Aggregate results from a full scan.
pub struct ScanResult {
    pub crates: usize,
    pub modules: usize,
    pub types: usize,
    pub functions: usize,
    pub tests: usize,
    pub snippets: usize,
    pub docs: usize,
    pub commits: usize,
    pub branches: usize,
    pub duration_ms: u64,
}

/// Run all scanners against the workspace.
pub fn full_scan(
    root: &Path,
    config: &ScanConfig,
    engine: &mut GraphEngine,
) -> Result<ScanResult> {
    let start = Instant::now();

    // Clear existing data for this worktree before re-scanning
    engine.clear_worktree(&config.worktree)?;

    // 1. Scan Rust code
    let mut total_crates = 0;
    let mut total_modules = 0;
    let mut total_types = 0;
    let mut total_functions = 0;
    let mut total_tests = 0;
    let mut total_snippets = 0;

    for rust_root in &config.rust_roots {
        let scan_path = root.join(rust_root);
        if !scan_path.exists() {
            continue;
        }
        let metrics =
            code::scan_rust_workspace(&scan_path, engine, &config.worktree)?;
        total_crates += metrics.crates_found;
        total_modules += metrics.modules_found;
        total_types += metrics.types_found;
        total_functions += metrics.functions_found;
        total_tests += metrics.tests_found;
        total_snippets += metrics.snippets_stored;
    }

    // 2. Scan docs
    let docs_count = docs::scan_docs(root, engine)?;

    // 3. Scan git
    let git_path = root.join(&config.git_repo);
    git::scan_git(&git_path, engine)?;

    let commits = engine
        .query_nodes(Some(crate::schema::NodeKind::Commit), None)?
        .len();
    let branches = engine
        .query_nodes(Some(crate::schema::NodeKind::Branch), None)?
        .len();

    let duration = start.elapsed();

    // 4. Create top-level system node for C4 context diagrams
    {
        use crate::schema::*;
        let now = Utc::now();
        let system_name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "PhiloticStack".to_string());
        let system_id = format!("system:{}", system_name);

        engine.upsert_node(&Node {
            id: system_id.clone(),
            kind: NodeKind::Component,
            name: system_name.clone(),
            properties: serde_json::json!({
                "system": true,
                "crates": total_crates,
                "modules": total_modules,
                "types": total_types,
                "functions": total_functions,
                "tests": total_tests,
                "docs": docs_count,
            }),
            file_path: Some(root.display().to_string()),
            worktree: config.worktree.clone(),
            created_at: now,
            updated_at: now,
            embedding: None,
            embedding_model: None,
            embedding_dims: None,
            embedding_updated: None,
            embedding_hash: None,
        })?;

        // Link system to all crates
        let crates = engine.query_nodes(Some(NodeKind::Crate), None)?;
        for c in &crates {
            engine.upsert_edge(&Edge {
                source_id: system_id.clone(),
                target_id: c.id.clone(),
                relation: EdgeRelation::Contains,
                properties: serde_json::json!({}),
                worktree: config.worktree.clone(),
            })?;
        }

        // Link system to all domains
        let domains = engine.query_nodes(Some(NodeKind::Domain), None)?;
        for d in &domains {
            engine.upsert_edge(&Edge {
                source_id: system_id.clone(),
                target_id: d.id.clone(),
                relation: EdgeRelation::Governs,
                properties: serde_json::json!({}),
                worktree: String::new(),
            })?;
        }
    }

    // 5. Auto-embed proposals (best effort, don't fail scan if this errors)
    // This runs synchronously for simplicity; could be made async in future
    if let Ok(proposals) = engine.query_nodes(Some(crate::schema::NodeKind::Proposal), None) {
        let unembedded: Vec<_> = proposals
            .into_iter()
            .filter(|n| n.embedding.is_none())
            .collect();
        if !unembedded.is_empty() {
            // Note: We can't actually do the embedding here without async runtime
            // and the ONNX client. For now, we log that there are unembedded nodes.
            // A background task or manual batch embed should be used.
            eprintln!(
                "[scan] Found {} unembedded proposals. Run: graph_embed_batch with kind=proposal",
                unembedded.len()
            );
        }
    }

    Ok(ScanResult {
        crates: total_crates,
        modules: total_modules,
        types: total_types,
        functions: total_functions,
        tests: total_tests,
        snippets: total_snippets,
        docs: docs_count,
        commits,
        branches,
        duration_ms: duration.as_millis() as u64,
    })
}
