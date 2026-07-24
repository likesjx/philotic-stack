use graph_intelligence::{full_scan, Edge, EdgeRelation, GraphEngine, Node, NodeKind, ScanConfig};
use std::path::PathBuf;

/// Find the workspace root by walking up from the current crate's manifest dir.
fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/graph-intelligence -> go up two levels to workspace root
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .expect("Could not find workspace root")
}

#[test]
fn test_full_workspace_scan() {
    let root = workspace_root();
    println!("Workspace root: {}", root.display());

    let mut engine = GraphEngine::open(":memory:").expect("Failed to create in-memory engine");

    let config = ScanConfig {
        rust_roots: vec!["crates".to_string()],
        doc_roots: vec!["docs".to_string()],
        git_repo: ".".to_string(),
        worktree: "develop".to_string(),
    };

    let result = full_scan(&root, &config, &mut engine).expect("Full scan failed");

    println!("\n=== Scan Results ===");
    println!("Crates:    {}", result.crates);
    println!("Modules:   {}", result.modules);
    println!("Types:     {}", result.types);
    println!("Functions: {}", result.functions);
    println!("Tests:     {}", result.tests);
    println!("Snippets:  {}", result.snippets);
    println!("Docs:      {}", result.docs);
    println!("Commits:   {}", result.commits);
    println!("Branches:  {}", result.branches);
    println!("Duration:  {}ms", result.duration_ms);

    // Assertions: we should find real data from the philotic-stack workspace
    assert!(result.crates > 0, "Expected > 0 crate nodes");
    assert!(result.modules > 0, "Expected > 0 module nodes");
    assert!(result.types > 0, "Expected > 0 type nodes");
    assert!(result.docs > 0, "Expected > 0 doc nodes from docs/");

    // Verify we can query specific node kinds
    let crate_nodes = engine
        .query_nodes(Some(NodeKind::Crate), None)
        .expect("Failed to query crate nodes");
    println!("\nCrate nodes found:");
    for node in &crate_nodes {
        println!("  - {} ({})", node.name, node.id);
    }
    assert!(!crate_nodes.is_empty());

    let proposal_nodes = engine
        .query_nodes(Some(NodeKind::Proposal), None)
        .expect("Failed to query proposal nodes");
    println!("\nProposal nodes found: {}", proposal_nodes.len());
    for node in proposal_nodes.iter().take(5) {
        println!("  - {} ({})", node.name, node.id);
    }
    assert!(
        !proposal_nodes.is_empty(),
        "Expected proposal nodes from docs"
    );

    // Verify seam nodes exist from SEAM_REGISTRY
    let seam_nodes = engine
        .query_nodes(Some(NodeKind::Seam), None)
        .expect("Failed to query seam nodes");
    println!("\nSeam nodes found: {}", seam_nodes.len());
    for node in seam_nodes.iter().take(5) {
        println!("  - {} ({})", node.name, node.id);
    }

    // Verify we have edges
    let edge_count = engine.count_edges().expect("Failed to count edges");
    println!("\nTotal edges: {}", edge_count);
    assert!(edge_count > 0, "Expected > 0 edges");

    // Verify snippets
    let snippet_count = engine.count_snippets().expect("Failed to count snippets");
    println!("Total snippets: {}", snippet_count);
    assert!(snippet_count > 0, "Expected > 0 snippets");

    println!("\n=== Scan test passed ===");
}

#[test]
fn test_engine_in_memory() {
    let engine = GraphEngine::open(":memory:").expect("Failed to open in-memory DB");

    let now = chrono::Utc::now();
    let node = graph_intelligence::Node {
        id: "test:1".into(),
        kind: NodeKind::Crate,
        name: "test-crate".into(),
        properties: serde_json::json!({}),
        file_path: None,
        worktree: "test".into(),
        created_at: now,
        updated_at: now,
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    };

    engine.upsert_node(&node).unwrap();
    let fetched = engine.get_node("test:1").unwrap().unwrap();
    assert_eq!(fetched.name, "test-crate");
    assert_eq!(fetched.kind, NodeKind::Crate);

    // Test upsert overwrites
    let updated = graph_intelligence::Node {
        id: "test:1".into(),
        kind: NodeKind::Crate,
        name: "updated-name".into(),
        properties: serde_json::json!({"updated": true}),
        file_path: None,
        worktree: "test".into(),
        created_at: now,
        updated_at: now,
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    };
    engine.upsert_node(&updated).unwrap();
    let fetched2 = engine.get_node("test:1").unwrap().unwrap();
    assert_eq!(fetched2.name, "updated-name");

    // Test delete
    engine.delete_node("test:1").unwrap();
    assert!(engine.get_node("test:1").unwrap().is_none());
}

#[test]
fn test_plantuml_generation() {
    let root = workspace_root();
    let mut engine = GraphEngine::open(":memory:").expect("Failed to create engine");

    let config = ScanConfig {
        rust_roots: vec!["crates".to_string()],
        doc_roots: vec!["docs".to_string()],
        git_repo: ".".to_string(),
        worktree: "develop".to_string(),
    };

    full_scan(&root, &config, &mut engine).expect("Scan failed");

    // Try to generate a diagram for any crate that was found
    let crate_nodes = engine
        .query_nodes(Some(NodeKind::Crate), None)
        .expect("Failed to query");

    if let Some(crate_node) = crate_nodes.first() {
        let diagram =
            graph_intelligence::plantuml::generate_crate_diagram(&engine, &crate_node.name)
                .expect("Failed to generate diagram");

        println!(
            "PlantUML for {}:\n{}",
            crate_node.name,
            &diagram[..diagram.len().min(500)]
        );
        assert!(diagram.contains("@startuml"));
        assert!(diagram.contains("@enduml"));
    }
}

#[test]
fn test_graph_authored_doc_content_survives_rescan() {
    let root = workspace_root();
    let mut engine = GraphEngine::open(":memory:").expect("Failed to create engine");

    let config = ScanConfig {
        rust_roots: vec!["crates".to_string()],
        doc_roots: vec!["docs".to_string()],
        git_repo: ".".to_string(),
        worktree: "develop".to_string(),
    };

    full_scan(&root, &config, &mut engine).expect("Initial scan failed");

    let proposal = engine
        .query_nodes(Some(NodeKind::Proposal), None)
        .expect("Failed to query proposals")
        .into_iter()
        .next()
        .expect("Expected at least one proposal");

    let mut updated = proposal.clone();
    let mut props = updated.properties.as_object().cloned().unwrap_or_default();
    props.insert(
        "content".to_string(),
        serde_json::json!("# Graph-owned content\n\nThis should survive rescan."),
    );
    props.insert("content_format".to_string(), serde_json::json!("markdown"));
    props.insert("content_source".to_string(), serde_json::json!("graph"));
    updated.properties = serde_json::Value::Object(props);
    updated.updated_at = chrono::Utc::now();
    engine
        .upsert_node(&updated)
        .expect("Failed to store graph content");

    full_scan(&root, &config, &mut engine).expect("Rescan failed");

    let rescanned = engine
        .get_node(&proposal.id)
        .expect("Failed to fetch rescanned proposal")
        .expect("Proposal should still exist after rescan");

    assert_eq!(
        rescanned.properties["content_source"],
        serde_json::json!("graph")
    );
    assert!(rescanned.properties["content"]
        .as_str()
        .unwrap_or_default()
        .contains("Graph-owned content"));
}

/// A doc-kind node authored directly in the graph (via `graph_create_node`) has
/// no backing file, so the scanner's on-disk rebuild loop can never recreate it.
/// `clear_scanned_doc_nodes` must therefore leave it — and its edges — alone.
/// Regression: before the `file_path IS NOT NULL` guard, such nodes were deleted
/// permanently and silently by the next scan (including the 6h freshness run).
#[test]
fn test_graph_authored_fileless_proposal_survives_rescan() {
    let root = workspace_root();
    let mut engine = GraphEngine::open(":memory:").expect("Failed to create engine");

    let config = ScanConfig {
        rust_roots: vec!["crates".to_string()],
        doc_roots: vec!["docs".to_string()],
        git_repo: ".".to_string(),
        worktree: "develop".to_string(),
    };

    full_scan(&root, &config, &mut engine).expect("Initial scan failed");

    // Mirror exactly what the `graph_create_node` MCP tool builds:
    // file_path: None, worktree: "".
    let authored = Node {
        id: "proposal:graph-native-authoring-test".to_string(),
        kind: NodeKind::Proposal,
        name: "Graph Native Authoring Test".to_string(),
        properties: serde_json::json!({
            "status": "proposed",
            "domain": "workflow-docs",
            "content": "# Authored in the graph\n\nNo markdown file backs this node.",
            "content_source": "graph",
        }),
        file_path: None,
        worktree: String::new(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    };
    engine
        .upsert_node(&authored)
        .expect("Failed to author file-less proposal");

    // An edge from the authored node to a scanned node must also survive.
    let scanned_proposal = engine
        .query_nodes(Some(NodeKind::Proposal), None)
        .expect("Failed to query proposals")
        .into_iter()
        .find(|n| n.file_path.is_some())
        .expect("Expected at least one file-backed proposal");
    engine
        .upsert_edge(&Edge {
            source_id: authored.id.clone(),
            target_id: scanned_proposal.id.clone(),
            relation: EdgeRelation::References,
            properties: serde_json::json!({}),
            worktree: String::new(),
        })
        .expect("Failed to create edge from authored node");

    full_scan(&root, &config, &mut engine).expect("Rescan failed");

    let survived = engine
        .get_node(&authored.id)
        .expect("Failed to fetch authored proposal")
        .expect("File-less graph-authored proposal must survive rescan");
    assert_eq!(survived.name, "Graph Native Authoring Test");
    assert_eq!(
        survived.properties["content_source"],
        serde_json::json!("graph")
    );
    assert!(survived.properties["content"]
        .as_str()
        .unwrap_or_default()
        .contains("Authored in the graph"));

    let edges = engine
        .get_edges_from(&authored.id)
        .expect("Failed to fetch edges for authored proposal");
    assert!(
        edges.iter().any(|e| e.target_id == scanned_proposal.id),
        "Edge from the graph-authored node must survive rescan"
    );
}
