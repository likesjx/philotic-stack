use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use quote::ToTokens;
use sha2::{Digest, Sha256};
use syn::{Attribute, Item, Visibility};
use walkdir::WalkDir;

use crate::engine::GraphEngine;
use crate::schema::*;

/// Metrics collected during a Rust workspace scan.
#[derive(Debug, Default)]
pub struct ScanMetrics {
    pub crates_found: usize,
    pub modules_found: usize,
    pub types_found: usize,
    pub functions_found: usize,
    pub tests_found: usize,
    pub snippets_stored: usize,
    pub impl_blocks: usize,
    pub edges_created: usize,
    pub parse_failures: usize,
    pub files_scanned: usize,
}

/// Scan a Rust workspace starting from `root`, looking under subdirectories for Cargo.toml files.
pub fn scan_rust_workspace(
    root: &Path,
    engine: &GraphEngine,
    worktree: &str,
) -> Result<ScanMetrics> {
    let mut metrics = ScanMetrics::default();
    let now = Utc::now();

    // Find all crate roots by locating Cargo.toml files
    let crate_roots = find_crate_roots(root);

    for (crate_name, crate_path) in &crate_roots {
        let crate_id = format!("crate:{}", crate_name);
        let crate_node = Node {
            id: crate_id.clone(),
            kind: NodeKind::Crate,
            name: crate_name.clone(),
            properties: serde_json::json!({
                "path": crate_path.display().to_string(),
            }),
            file_path: Some(crate_path.join("Cargo.toml").display().to_string()),
            worktree: worktree.to_string(),
            created_at: now,
                embedding: None,
                embedding_model: None,
                embedding_dims: None,
                embedding_updated: None,
                embedding_hash: None,
            updated_at: now,
        };
        engine.upsert_node(&crate_node)?;
        metrics.crates_found += 1;

        // Scan all .rs files in the crate's src directory
        let src_dir = crate_path.join("src");
        if !src_dir.exists() {
            continue;
        }

        for entry in WalkDir::new(&src_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path().extension().map_or(false, |ext| ext == "rs")
            })
        {
            let file_path = entry.path();
            let rel_path = file_path
                .strip_prefix(root)
                .unwrap_or(file_path)
                .display()
                .to_string();

            let module_path = file_to_module_path(file_path, crate_name, &src_dir);
            let module_id = format!("module:{}", module_path);

            let module_node = Node {
                id: module_id.clone(),
                kind: NodeKind::Module,
                name: module_path.clone(),
                properties: serde_json::json!({}),
                file_path: Some(rel_path.clone()),
                worktree: worktree.to_string(),
                created_at: now,
                embedding: None,
                embedding_model: None,
                embedding_dims: None,
                embedding_updated: None,
                embedding_hash: None,
                updated_at: now,
            };
            engine.upsert_node(&module_node)?;
            metrics.modules_found += 1;

            // Crate contains module
            engine.upsert_edge(&Edge {
                source_id: crate_id.clone(),
                target_id: module_id.clone(),
                relation: EdgeRelation::Contains,
                properties: serde_json::json!({}),
                worktree: worktree.to_string(),
            })?;
            metrics.edges_created += 1;

            // Parse the file
            let source = match fs::read_to_string(file_path) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let file_ast = match syn::parse_file(&source) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!(
                        "Warning: failed to parse {}: {}",
                        rel_path, e
                    );
                    metrics.parse_failures += 1;
                    continue;
                }
            };
            metrics.files_scanned += 1;

            // Count LOC and unwraps for metrics
            let loc = source.lines().count();
            let unwrap_count = source.matches(".unwrap()").count();

            // Update module properties with metrics
            let module_with_metrics = Node {
                id: module_id.clone(),
                kind: NodeKind::Module,
                name: module_path.clone(),
                properties: serde_json::json!({
                    "loc": loc,
                    "unwrap_count": unwrap_count,
                }),
                file_path: Some(rel_path.clone()),
                worktree: worktree.to_string(),
                created_at: now,
                embedding: None,
                embedding_model: None,
                embedding_dims: None,
                embedding_updated: None,
                embedding_hash: None,
                updated_at: now,
            };
            engine.upsert_node(&module_with_metrics)?;

            // Process each item in the file
            scan_items(
                &file_ast.items,
                &module_id,
                &rel_path,
                worktree,
                &source,
                engine,
                &mut metrics,
                now,
            )?;
        }
    }

    Ok(metrics)
}

/// Scan a list of syn Items, extracting types, functions, impl blocks, tests, etc.
fn scan_items(
    items: &[Item],
    module_id: &str,
    file_path: &str,
    worktree: &str,
    source: &str,
    engine: &GraphEngine,
    metrics: &mut ScanMetrics,
    now: chrono::DateTime<Utc>,
) -> Result<()> {
    for item in items {
        match item {
            Item::Struct(s) => {
                let name = s.ident.to_string();
                let node_id = format!("{}::{}", module_id, name);
                let doc = extract_doc_comment(&s.attrs);
                let sig = item.to_token_stream().to_string();
                // Truncate signature to just the struct header + fields summary
                let signature = format_struct_signature(s);
                let (start, end) = span_lines(s.ident.span(), source);

                engine.upsert_node(&Node {
                    id: node_id.clone(),
                    kind: NodeKind::Type,
                    name: name.clone(),
                    properties: serde_json::json!({"type_kind": "struct"}),
                    file_path: Some(file_path.to_string()),
                    worktree: worktree.to_string(),
                    created_at: now,
                embedding: None,
                embedding_model: None,
                embedding_dims: None,
                embedding_updated: None,
                embedding_hash: None,
                    updated_at: now,
                })?;
                metrics.types_found += 1;

                engine.upsert_edge(&Edge {
                    source_id: module_id.to_string(),
                    target_id: node_id.clone(),
                    relation: EdgeRelation::Contains,
                    properties: serde_json::json!({}),
                    worktree: worktree.to_string(),
                })?;
                metrics.edges_created += 1;

                let body_str = sig.clone();
                engine.upsert_snippet(&Snippet {
                    id: format!("snippet:{}", node_id),
                    node_id: node_id.clone(),
                    kind: SnippetKind::Struct,
                    signature,
                    doc_comment: doc,
                    body: Some(body_str.clone()),
                    body_hash: hash_string(&body_str),
                    file_path: file_path.to_string(),
                    line_start: start,
                    line_end: end,
                    language: "rust".to_string(),
                })?;
                metrics.snippets_stored += 1;
            }

            Item::Trait(t) => {
                let name = t.ident.to_string();
                let node_id = format!("{}::{}", module_id, name);
                let doc = extract_doc_comment(&t.attrs);
                let signature = format_trait_signature(t);
                let (start, end) = span_lines(t.ident.span(), source);

                engine.upsert_node(&Node {
                    id: node_id.clone(),
                    kind: NodeKind::Type,
                    name: name.clone(),
                    properties: serde_json::json!({"type_kind": "trait"}),
                    file_path: Some(file_path.to_string()),
                    worktree: worktree.to_string(),
                    created_at: now,
                embedding: None,
                embedding_model: None,
                embedding_dims: None,
                embedding_updated: None,
                embedding_hash: None,
                    updated_at: now,
                })?;
                metrics.types_found += 1;

                engine.upsert_edge(&Edge {
                    source_id: module_id.to_string(),
                    target_id: node_id.clone(),
                    relation: EdgeRelation::Contains,
                    properties: serde_json::json!({}),
                    worktree: worktree.to_string(),
                })?;
                metrics.edges_created += 1;

                let body_str = item.to_token_stream().to_string();
                engine.upsert_snippet(&Snippet {
                    id: format!("snippet:{}", node_id),
                    node_id: node_id.clone(),
                    kind: SnippetKind::Trait,
                    signature,
                    doc_comment: doc,
                    body: Some(body_str.clone()),
                    body_hash: hash_string(&body_str),
                    file_path: file_path.to_string(),
                    line_start: start,
                    line_end: end,
                    language: "rust".to_string(),
                })?;
                metrics.snippets_stored += 1;
            }

            Item::Enum(e) => {
                let name = e.ident.to_string();
                let node_id = format!("{}::{}", module_id, name);
                let doc = extract_doc_comment(&e.attrs);
                let signature = format_enum_signature(e);
                let (start, end) = span_lines(e.ident.span(), source);

                engine.upsert_node(&Node {
                    id: node_id.clone(),
                    kind: NodeKind::Type,
                    name: name.clone(),
                    properties: serde_json::json!({"type_kind": "enum"}),
                    file_path: Some(file_path.to_string()),
                    worktree: worktree.to_string(),
                    created_at: now,
                embedding: None,
                embedding_model: None,
                embedding_dims: None,
                embedding_updated: None,
                embedding_hash: None,
                    updated_at: now,
                })?;
                metrics.types_found += 1;

                engine.upsert_edge(&Edge {
                    source_id: module_id.to_string(),
                    target_id: node_id.clone(),
                    relation: EdgeRelation::Contains,
                    properties: serde_json::json!({}),
                    worktree: worktree.to_string(),
                })?;
                metrics.edges_created += 1;

                let body_str = item.to_token_stream().to_string();
                engine.upsert_snippet(&Snippet {
                    id: format!("snippet:{}", node_id),
                    node_id: node_id.clone(),
                    kind: SnippetKind::Enum,
                    signature,
                    doc_comment: doc,
                    body: Some(body_str.clone()),
                    body_hash: hash_string(&body_str),
                    file_path: file_path.to_string(),
                    line_start: start,
                    line_end: end,
                    language: "rust".to_string(),
                })?;
                metrics.snippets_stored += 1;
            }

            Item::Fn(f) => {
                let name = f.sig.ident.to_string();
                let is_test = has_test_attr(&f.attrs);
                let node_kind = if is_test {
                    NodeKind::Test
                } else {
                    NodeKind::Function
                };
                let snippet_kind = if is_test {
                    SnippetKind::Test
                } else {
                    SnippetKind::Function
                };
                let node_id = format!("{}::{}", module_id, name);
                let doc = extract_doc_comment(&f.attrs);
                let signature = f.sig.to_token_stream().to_string();
                let (start, end) = span_lines(f.sig.ident.span(), source);

                engine.upsert_node(&Node {
                    id: node_id.clone(),
                    kind: node_kind,
                    name: name.clone(),
                    properties: serde_json::json!({
                        "is_async": f.sig.asyncness.is_some(),
                        "is_pub": matches!(f.vis, Visibility::Public(_)),
                    }),
                    file_path: Some(file_path.to_string()),
                    worktree: worktree.to_string(),
                    created_at: now,
                embedding: None,
                embedding_model: None,
                embedding_dims: None,
                embedding_updated: None,
                embedding_hash: None,
                    updated_at: now,
                })?;
                if is_test {
                    metrics.tests_found += 1;
                } else {
                    metrics.functions_found += 1;
                }

                engine.upsert_edge(&Edge {
                    source_id: module_id.to_string(),
                    target_id: node_id.clone(),
                    relation: if is_test {
                        EdgeRelation::Tests
                    } else {
                        EdgeRelation::Contains
                    },
                    properties: serde_json::json!({}),
                    worktree: worktree.to_string(),
                })?;
                metrics.edges_created += 1;

                let body_str = f.block.to_token_stream().to_string();
                engine.upsert_snippet(&Snippet {
                    id: format!("snippet:{}", node_id),
                    node_id: node_id.clone(),
                    kind: snippet_kind,
                    signature,
                    doc_comment: doc,
                    body: None, // Don't store function bodies — just hash them
                    body_hash: hash_string(&body_str),
                    file_path: file_path.to_string(),
                    line_start: start,
                    line_end: end,
                    language: "rust".to_string(),
                })?;
                metrics.snippets_stored += 1;
            }

            Item::Impl(imp) => {
                let self_ty = imp.self_ty.to_token_stream().to_string();
                let trait_name = imp.trait_.as_ref().map(|(_, path, _)| {
                    path.to_token_stream().to_string()
                });

                let impl_label = match &trait_name {
                    Some(t) => format!("impl {} for {}", t, self_ty),
                    None => format!("impl {}", self_ty),
                };
                let impl_id = format!("{}::impl::{}", module_id, sanitize_id(&impl_label));

                engine.upsert_node(&Node {
                    id: impl_id.clone(),
                    kind: NodeKind::ImplBlock,
                    name: impl_label.clone(),
                    properties: serde_json::json!({
                        "self_type": self_ty,
                        "trait": trait_name,
                    }),
                    file_path: Some(file_path.to_string()),
                    worktree: worktree.to_string(),
                    created_at: now,
                embedding: None,
                embedding_model: None,
                embedding_dims: None,
                embedding_updated: None,
                embedding_hash: None,
                    updated_at: now,
                })?;
                metrics.impl_blocks += 1;

                // If it's a trait impl, create a TraitImpl edge
                if let Some(trait_n) = &imp.trait_ {
                    let trait_path = trait_n.1.to_token_stream().to_string();
                    let trait_type_id = format!("{}::{}", module_id, sanitize_id(&trait_path));
                    engine.upsert_edge(&Edge {
                        source_id: impl_id.clone(),
                        target_id: trait_type_id,
                        relation: EdgeRelation::TraitImpl,
                        properties: serde_json::json!({"for_type": self_ty}),
                        worktree: worktree.to_string(),
                    })?;
                    metrics.edges_created += 1;
                }

                // Module contains impl block
                engine.upsert_edge(&Edge {
                    source_id: module_id.to_string(),
                    target_id: impl_id.clone(),
                    relation: EdgeRelation::Contains,
                    properties: serde_json::json!({}),
                    worktree: worktree.to_string(),
                })?;
                metrics.edges_created += 1;

                // Scan methods inside the impl block
                for impl_item in &imp.items {
                    if let syn::ImplItem::Fn(method) = impl_item {
                        let method_name = method.sig.ident.to_string();
                        let is_test = has_test_attr(&method.attrs);
                        let method_id = format!("{}::{}", impl_id, method_name);
                        let doc = extract_doc_comment(&method.attrs);
                        let signature = method.sig.to_token_stream().to_string();
                        let (start, end) = span_lines(method.sig.ident.span(), source);

                        let node_kind = if is_test {
                            NodeKind::Test
                        } else {
                            NodeKind::Function
                        };

                        engine.upsert_node(&Node {
                            id: method_id.clone(),
                            kind: node_kind,
                            name: method_name.clone(),
                            properties: serde_json::json!({
                                "is_async": method.sig.asyncness.is_some(),
                                "impl_block": impl_label,
                            }),
                            file_path: Some(file_path.to_string()),
                            worktree: worktree.to_string(),
                            created_at: now,
                embedding: None,
                embedding_model: None,
                embedding_dims: None,
                embedding_updated: None,
                embedding_hash: None,
                            updated_at: now,
                        })?;
                        if is_test {
                            metrics.tests_found += 1;
                        } else {
                            metrics.functions_found += 1;
                        }

                        engine.upsert_edge(&Edge {
                            source_id: impl_id.clone(),
                            target_id: method_id.clone(),
                            relation: EdgeRelation::Contains,
                            properties: serde_json::json!({}),
                            worktree: worktree.to_string(),
                        })?;
                        metrics.edges_created += 1;

                        let body_str = method.block.to_token_stream().to_string();
                        engine.upsert_snippet(&Snippet {
                            id: format!("snippet:{}", method_id),
                            node_id: method_id.clone(),
                            kind: if is_test {
                                SnippetKind::Test
                            } else {
                                SnippetKind::Function
                            },
                            signature,
                            doc_comment: doc,
                            body: None,
                            body_hash: hash_string(&body_str),
                            file_path: file_path.to_string(),
                            line_start: start,
                            line_end: end,
                            language: "rust".to_string(),
                        })?;
                        metrics.snippets_stored += 1;
                    }
                }
            }

            Item::Use(u) => {
                // Create import edges from module to the imported path
                let use_path = u.to_token_stream().to_string();
                // We record a simplified edge for top-level use statements
                if let Some(imported_crate) = extract_use_crate(&u.tree) {
                    engine.upsert_edge(&Edge {
                        source_id: module_id.to_string(),
                        target_id: format!("crate:{}", imported_crate),
                        relation: EdgeRelation::Imports,
                        properties: serde_json::json!({"full_path": use_path}),
                        worktree: worktree.to_string(),
                    })?;
                    metrics.edges_created += 1;
                }
            }

            Item::Const(c) => {
                let name = c.ident.to_string();
                let node_id = format!("{}::const::{}", module_id, name);
                let signature = format!("const {}: {}", name, c.ty.to_token_stream());
                let body_str = c.expr.to_token_stream().to_string();
                let (start, end) = span_lines(c.ident.span(), source);

                engine.upsert_snippet(&Snippet {
                    id: format!("snippet:{}", node_id),
                    node_id: module_id.to_string(),
                    kind: SnippetKind::Const,
                    signature,
                    doc_comment: extract_doc_comment(&c.attrs),
                    body: Some(body_str.clone()),
                    body_hash: hash_string(&body_str),
                    file_path: file_path.to_string(),
                    line_start: start,
                    line_end: end,
                    language: "rust".to_string(),
                })?;
                metrics.snippets_stored += 1;
            }

            Item::Static(s) => {
                let name = s.ident.to_string();
                let node_id = format!("{}::static::{}", module_id, name);
                let signature = format!("static {}: {}", name, s.ty.to_token_stream());
                let body_str = s.expr.to_token_stream().to_string();
                let (start, end) = span_lines(s.ident.span(), source);

                engine.upsert_snippet(&Snippet {
                    id: format!("snippet:{}", node_id),
                    node_id: module_id.to_string(),
                    kind: SnippetKind::Static,
                    signature,
                    doc_comment: extract_doc_comment(&s.attrs),
                    body: Some(body_str.clone()),
                    body_hash: hash_string(&body_str),
                    file_path: file_path.to_string(),
                    line_start: start,
                    line_end: end,
                    language: "rust".to_string(),
                })?;
                metrics.snippets_stored += 1;
            }

            Item::Type(t) => {
                let name = t.ident.to_string();
                let node_id = format!("{}::{}", module_id, name);
                let signature = format!("type {} = {}", name, t.ty.to_token_stream());
                let (start, end) = span_lines(t.ident.span(), source);

                engine.upsert_node(&Node {
                    id: node_id.clone(),
                    kind: NodeKind::Type,
                    name: name.clone(),
                    properties: serde_json::json!({"type_kind": "alias"}),
                    file_path: Some(file_path.to_string()),
                    worktree: worktree.to_string(),
                    created_at: now,
                embedding: None,
                embedding_model: None,
                embedding_dims: None,
                embedding_updated: None,
                embedding_hash: None,
                    updated_at: now,
                })?;
                metrics.types_found += 1;

                engine.upsert_snippet(&Snippet {
                    id: format!("snippet:{}", node_id),
                    node_id: node_id.clone(),
                    kind: SnippetKind::TypeAlias,
                    signature,
                    doc_comment: extract_doc_comment(&t.attrs),
                    body: None,
                    body_hash: hash_string(&t.ty.to_token_stream().to_string()),
                    file_path: file_path.to_string(),
                    line_start: start,
                    line_end: end,
                    language: "rust".to_string(),
                })?;
                metrics.snippets_stored += 1;
            }

            Item::Macro(m) => {
                if let Some(ident) = &m.ident {
                    let name = ident.to_string();
                    let node_id = format!("{}::macro::{}", module_id, name);
                    let signature = format!("macro_rules! {}", name);
                    let body_str = m.mac.tokens.to_string();
                    let (start, end) = span_lines(ident.span(), source);

                    engine.upsert_snippet(&Snippet {
                        id: format!("snippet:{}", node_id),
                        node_id: module_id.to_string(),
                        kind: SnippetKind::Macro,
                        signature,
                        doc_comment: extract_doc_comment(&m.attrs),
                        body: None,
                        body_hash: hash_string(&body_str),
                        file_path: file_path.to_string(),
                        line_start: start,
                        line_end: end,
                        language: "rust".to_string(),
                    })?;
                    metrics.snippets_stored += 1;
                }
            }

            Item::Mod(m) => {
                // Inline modules — recurse into their items
                if let Some((_, items)) = &m.content {
                    let sub_mod = format!("{}::{}", module_id, m.ident);
                    let sub_mod_node = Node {
                        id: sub_mod.clone(),
                        kind: NodeKind::Module,
                        name: format!("{}::{}", module_id.strip_prefix("module:").unwrap_or(module_id), m.ident),
                        properties: serde_json::json!({"inline": true}),
                        file_path: Some(file_path.to_string()),
                        worktree: worktree.to_string(),
                        created_at: now,
                embedding: None,
                embedding_model: None,
                embedding_dims: None,
                embedding_updated: None,
                embedding_hash: None,
                        updated_at: now,
                    };
                    engine.upsert_node(&sub_mod_node)?;
                    engine.upsert_edge(&Edge {
                        source_id: module_id.to_string(),
                        target_id: sub_mod.clone(),
                        relation: EdgeRelation::Contains,
                        properties: serde_json::json!({}),
                        worktree: worktree.to_string(),
                    })?;
                    metrics.edges_created += 1;
                    metrics.modules_found += 1;

                    scan_items(items, &sub_mod, file_path, worktree, source, engine, metrics, now)?;
                }
            }

            _ => {}
        }
    }
    Ok(())
}

// ── Helpers ──

fn find_crate_roots(root: &Path) -> Vec<(String, PathBuf)> {
    let mut crates = Vec::new();
    for entry in WalkDir::new(root)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_name() == "Cargo.toml" && entry.path() != root.join("Cargo.toml") {
            let crate_dir = entry.path().parent().unwrap().to_path_buf();
            // Try to extract the crate name from the directory name
            let name = crate_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if !name.is_empty() {
                crates.push((name, crate_dir));
            }
        }
    }
    crates
}

fn file_to_module_path(file: &Path, crate_name: &str, src_dir: &Path) -> String {
    let rel = file.strip_prefix(src_dir).unwrap_or(file);
    let stem = rel
        .with_extension("")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("::");

    // lib.rs or main.rs → just the crate name
    if stem == "lib" || stem == "main" {
        crate_name.to_string()
    } else {
        // Strip trailing ::mod
        let cleaned = if stem.ends_with("::mod") {
            &stem[..stem.len() - 5]
        } else {
            &stem
        };
        format!("{}::{}", crate_name, cleaned)
    }
}

fn extract_doc_comment(attrs: &[Attribute]) -> Option<String> {
    let mut docs = Vec::new();
    for attr in attrs {
        if attr.path().is_ident("doc") {
            if let syn::Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(lit) = &nv.value {
                    if let syn::Lit::Str(s) = &lit.lit {
                        docs.push(s.value().trim().to_string());
                    }
                }
            }
        }
    }
    if docs.is_empty() {
        None
    } else {
        Some(docs.join("\n"))
    }
}

fn has_test_attr(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("test")
            || attr.path().is_ident("tokio::test")
            || (attr.path().segments.len() == 2
                && attr.path().segments.last().map_or(false, |s| s.ident == "test"))
    })
}

fn hash_string(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hex::encode(hasher.finalize())
}

fn span_lines(span: proc_macro2::Span, source: &str) -> (u32, u32) {
    let start = span.start().line as u32;
    let end = span.end().line as u32;
    // span-locations gives the span of the identifier only — estimate the
    // block end by scanning forward for a reasonable range. If start==end
    // (single-line item like a field), just return that line.
    if start == 0 {
        // Fallback if span locations aren't available
        let lines = source.lines().count() as u32;
        return (1, lines);
    }
    if end > start {
        (start, end)
    } else {
        (start, start)
    }
}

fn format_struct_signature(s: &syn::ItemStruct) -> String {
    let vis = s.vis.to_token_stream().to_string();
    let name = s.ident.to_string();
    let fields: Vec<String> = match &s.fields {
        syn::Fields::Named(named) => named
            .named
            .iter()
            .map(|f| {
                let fname = f
                    .ident
                    .as_ref()
                    .map(|i| i.to_string())
                    .unwrap_or_default();
                let ty = f.ty.to_token_stream().to_string();
                format!("    {}: {}", fname, ty)
            })
            .collect(),
        syn::Fields::Unnamed(unnamed) => unnamed
            .unnamed
            .iter()
            .enumerate()
            .map(|(i, f)| format!("    {}: {}", i, f.ty.to_token_stream()))
            .collect(),
        syn::Fields::Unit => vec![],
    };
    if fields.is_empty() {
        format!("{} struct {}", vis, name).trim().to_string()
    } else {
        format!(
            "{} struct {} {{\n{}\n}}",
            vis,
            name,
            fields.join(",\n")
        )
        .trim()
        .to_string()
    }
}

fn format_trait_signature(t: &syn::ItemTrait) -> String {
    let vis = t.vis.to_token_stream().to_string();
    let name = t.ident.to_string();
    let methods: Vec<String> = t
        .items
        .iter()
        .filter_map(|item| {
            if let syn::TraitItem::Fn(m) = item {
                Some(format!("    {};", m.sig.to_token_stream()))
            } else {
                None
            }
        })
        .collect();
    if methods.is_empty() {
        format!("{} trait {}", vis, name).trim().to_string()
    } else {
        format!(
            "{} trait {} {{\n{}\n}}",
            vis,
            name,
            methods.join("\n")
        )
        .trim()
        .to_string()
    }
}

fn format_enum_signature(e: &syn::ItemEnum) -> String {
    let vis = e.vis.to_token_stream().to_string();
    let name = e.ident.to_string();
    let variants: Vec<String> = e
        .variants
        .iter()
        .map(|v| format!("    {}", v.ident))
        .collect();
    if variants.is_empty() {
        format!("{} enum {}", vis, name).trim().to_string()
    } else {
        format!(
            "{} enum {} {{\n{}\n}}",
            vis,
            name,
            variants.join(",\n")
        )
        .trim()
        .to_string()
    }
}

fn sanitize_id(s: &str) -> String {
    s.replace(' ', "_")
        .replace('<', "_")
        .replace('>', "_")
        .replace("::", "_")
}

fn extract_use_crate(tree: &syn::UseTree) -> Option<String> {
    match tree {
        syn::UseTree::Path(p) => {
            let ident = p.ident.to_string();
            // Skip standard library crates
            if ident == "std" || ident == "core" || ident == "alloc" || ident == "self" || ident == "super" || ident == "crate" {
                None
            } else {
                Some(ident)
            }
        }
        syn::UseTree::Name(n) => {
            let ident = n.ident.to_string();
            if ident == "std" || ident == "core" || ident == "alloc" || ident == "self" || ident == "super" || ident == "crate" {
                None
            } else {
                Some(ident)
            }
        }
        syn::UseTree::Group(g) => {
            // Return first non-std crate from the group
            for tree in &g.items {
                if let Some(c) = extract_use_crate(tree) {
                    return Some(c);
                }
            }
            None
        }
        syn::UseTree::Glob(_) => None,
        syn::UseTree::Rename(r) => {
            let ident = r.ident.to_string();
            if ident == "std" || ident == "core" || ident == "alloc" {
                None
            } else {
                Some(ident)
            }
        }
    }
}
