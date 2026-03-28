pub mod code;
pub mod docs;
pub mod git;

use std::path::Path;
use std::time::Instant;

use anyhow::Result;

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
