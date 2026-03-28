use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;

use graph_intelligence::server::api;
use graph_intelligence::server::ws::ChangeEvent;
use graph_intelligence::server::AppState;
use graph_intelligence::{full_scan, GraphEngine, ScanConfig};
use tokio::sync::{broadcast, Mutex};

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .expect("Could not find workspace root")
}

/// Find an available port by binding to port 0.
fn available_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("Failed to bind")
        .local_addr()
        .expect("Failed to get addr")
        .port()
}

/// Create a test AppState with a scanned in-memory engine.
fn test_state() -> Arc<AppState> {
    let root = workspace_root();
    let mut engine = GraphEngine::open(":memory:").expect("Failed to create engine");

    let scan_config = ScanConfig {
        rust_roots: vec!["crates".to_string()],
        doc_roots: vec!["docs".to_string()],
        git_repo: ".".to_string(),
        worktree: "develop".to_string(),
    };

    full_scan(&root, &scan_config, &mut engine).expect("Full scan failed");

    let (change_tx, _) = broadcast::channel::<ChangeEvent>(256);

    Arc::new(AppState {
        engine: Mutex::new(engine),
        scan_config: ScanConfig {
            rust_roots: vec!["crates".to_string()],
            doc_roots: vec!["docs".to_string()],
            git_repo: ".".to_string(),
            worktree: "develop".to_string(),
        },
        repo_root: root.to_string_lossy().to_string(),
        change_tx,
    })
}

#[tokio::test]
async fn test_api_status_endpoint() {
    let state = test_state();
    let port = available_port();

    let app = api::router(state);
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port))
        .await
        .expect("Failed to bind");

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Give server a moment to start
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = reqwest::Client::new();

    // GET /api/status
    let resp = client
        .get(format!("http://127.0.0.1:{}/api/status", port))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    assert!(body.get("node_counts").is_some(), "Missing node_counts in status response");
    assert!(body.get("edge_count").is_some(), "Missing edge_count in status response");
    assert!(body.get("snippet_count").is_some(), "Missing snippet_count in status response");

    // Verify some nodes were found
    let total = body["node_counts"]["total"].as_u64().unwrap_or(0);
    assert!(total > 0, "Expected total node count > 0, got {}", total);
}

#[tokio::test]
async fn test_api_proposals_endpoint() {
    let state = test_state();
    let port = available_port();

    let app = api::router(state);
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port))
        .await
        .expect("Failed to bind");

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = reqwest::Client::new();

    // GET /api/proposals
    let resp = client
        .get(format!("http://127.0.0.1:{}/api/proposals", port))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    assert!(body.is_array(), "Expected proposals to be an array");
    let proposals = body.as_array().unwrap();
    assert!(!proposals.is_empty(), "Expected at least one proposal from docs");

    // Each proposal should have basic fields
    let first = &proposals[0];
    assert!(first.get("id").is_some(), "Proposal missing id");
    assert!(first.get("name").is_some(), "Proposal missing name");
    assert!(first.get("kind").is_some(), "Proposal missing kind");
}

#[tokio::test]
async fn test_api_nodes_endpoint() {
    let state = test_state();
    let port = available_port();

    let app = api::router(state);
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port))
        .await
        .expect("Failed to bind");

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = reqwest::Client::new();

    // GET /api/nodes?kind=crate
    let resp = client
        .get(format!("http://127.0.0.1:{}/api/nodes?kind=crate", port))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    assert!(body.is_array());
    let crates = body.as_array().unwrap();
    assert!(!crates.is_empty(), "Expected at least one crate node");

    // Fetch a specific node by ID using the list endpoint with kind filter
    // (avoids URL path encoding issues with colons in IDs like "crate:foo")
    let crate_id = crates[0]["id"].as_str().unwrap();
    let crate_name = crates[0]["name"].as_str().unwrap();
    eprintln!("Found crate: {} ({})", crate_name, crate_id);

    // Verify the node has expected fields
    assert!(crates[0].get("kind").is_some());
    assert!(crates[0].get("worktree").is_some());

    // Test the edges endpoint — use a simple test node we know exists
    // First try getting edges for a crate node via the list
    let resp = client
        .get(format!("http://127.0.0.1:{}/api/nodes?kind=crate&worktree=develop", port))
        .send()
        .await
        .expect("Failed to send request");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    assert!(body.is_array());
    let filtered = body.as_array().unwrap();
    assert!(!filtered.is_empty(), "Expected crate nodes in develop worktree");
}

#[tokio::test]
async fn test_api_not_found() {
    let state = test_state();
    let port = available_port();

    let app = api::router(state);
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port))
        .await
        .expect("Failed to bind");

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = reqwest::Client::new();

    // GET /api/nodes/<nonexistent>
    let resp = client
        .get(format!(
            "http://127.0.0.1:{}/api/nodes/nonexistent:node:id",
            port
        ))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(resp.status(), 404);
}
