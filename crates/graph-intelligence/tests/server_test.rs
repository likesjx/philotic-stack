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
        server_info: serde_json::json!({"version": "test"}),
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
    assert!(
        body.get("node_counts").is_some(),
        "Missing node_counts in status response"
    );
    assert!(
        body.get("edge_count").is_some(),
        "Missing edge_count in status response"
    );
    assert!(
        body.get("snippet_count").is_some(),
        "Missing snippet_count in status response"
    );

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
    assert!(
        !proposals.is_empty(),
        "Expected at least one proposal from docs"
    );

    // Each proposal should have basic fields
    let first = &proposals[0];
    assert!(first.get("id").is_some(), "Proposal missing id");
    assert!(first.get("name").is_some(), "Proposal missing name");
    assert!(first.get("kind").is_some(), "Proposal missing kind");
}

#[tokio::test]
async fn test_api_proposal_content_prefers_graph_authored_content() {
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
    let proposals: serde_json::Value = client
        .get(format!("http://127.0.0.1:{}/api/proposals", port))
        .send()
        .await
        .expect("Failed to send request")
        .json()
        .await
        .expect("Failed to parse proposals");

    let proposal_id = proposals[0]["id"]
        .as_str()
        .expect("proposal id")
        .to_string();

    let update_resp = client
        .post(format!(
            "http://127.0.0.1:{}/api/nodes/{}/content",
            port, proposal_id
        ))
        .json(&serde_json::json!({
            "content": "# Graph-owned proposal body\n\nThis content should win over file fallback.",
            "format": "markdown",
            "agent": "test",
            "reason": "test graph-authored content"
        }))
        .send()
        .await
        .expect("Failed to update content");

    assert_eq!(update_resp.status(), 200);

    let content_resp: serde_json::Value = client
        .get(format!(
            "http://127.0.0.1:{}/api/proposals/{}/content",
            port, proposal_id
        ))
        .send()
        .await
        .expect("Failed to load proposal content")
        .json()
        .await
        .expect("Failed to parse content response");

    assert_eq!(content_resp["content_source"], "graph");
    assert!(content_resp["content"]
        .as_str()
        .unwrap_or_default()
        .contains("Graph-owned proposal body"));
}

#[tokio::test]
async fn test_api_manage_proposal_updates_agent_work_focus() {
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
    let proposals: serde_json::Value = client
        .get(format!("http://127.0.0.1:{}/api/proposals", port))
        .send()
        .await
        .expect("Failed to send request")
        .json()
        .await
        .expect("Failed to parse proposals");

    let proposal_id = proposals[0]["id"].as_str().expect("proposal id");
    let resp = client
        .post(format!(
            "http://127.0.0.1:{}/api/proposals/{}/manage",
            port, proposal_id
        ))
        .json(&serde_json::json!({
            "agent": "codex-test",
            "session": "session:test-manage-proposal",
            "status": "accepted-current-slice",
            "current_goal": "Track proposals in intel-graph",
            "observation": "Agent focus records bridge active work and shared proposal truth.",
            "reason": "test proposal management endpoint"
        }))
        .send()
        .await
        .expect("Failed to manage proposal");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("Failed to parse response");
    assert_eq!(body["managed"], true);
    assert_eq!(
        body["proposal"]["properties"]["status"],
        "accepted-current-slice"
    );
    assert_eq!(body["work_focus"]["kind"], "agent_work_focus");
    assert_eq!(
        body["work_focus"]["properties"]["current_goal"],
        "Track proposals in intel-graph"
    );
    assert!(body["mutation_id"]
        .as_str()
        .unwrap_or_default()
        .starts_with("mut:"));
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
        .get(format!(
            "http://127.0.0.1:{}/api/nodes?kind=crate&worktree=develop",
            port
        ))
        .send()
        .await
        .expect("Failed to send request");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    assert!(body.is_array());
    let filtered = body.as_array().unwrap();
    assert!(
        !filtered.is_empty(),
        "Expected crate nodes in develop worktree"
    );
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

// ── New endpoint tests: context, session lifecycle, decide, dashboard, next_task, impact ──

/// Helper: spin up a test server, return (port, client)
async fn start_test_server() -> (u16, reqwest::Client) {
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
    (port, reqwest::Client::new())
}

#[tokio::test]
async fn test_api_next_task() {
    let (port, client) = start_test_server().await;

    let resp = client
        .get(format!("http://127.0.0.1:{}/api/next_task", port))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    // Should return either a task with an id or a "no work" message
    assert!(
        body.get("id").is_some() || body.get("message").is_some(),
        "Expected id or message in next_task response, got: {:?}",
        body
    );
}

#[tokio::test]
async fn test_api_dashboard() {
    let (port, client) = start_test_server().await;

    let resp = client
        .get(format!("http://127.0.0.1:{}/api/dashboard", port))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    assert!(
        body.get("active_sessions").is_some(),
        "Missing active_sessions"
    );
    assert!(body.get("agents").is_some(), "Missing agents");
    assert!(
        body.get("total_proposals").is_some(),
        "Missing total_proposals"
    );
    assert!(
        body.get("total_sessions").is_some(),
        "Missing total_sessions"
    );
    assert!(
        body.get("total_decisions").is_some(),
        "Missing total_decisions"
    );
    assert!(
        body.get("verification_progress").is_some(),
        "Missing verification_progress"
    );
}

#[tokio::test]
async fn test_api_context_for_proposal() {
    let (port, client) = start_test_server().await;

    // First, find a real proposal ID from the scanned graph
    let resp = client
        .get(format!("http://127.0.0.1:{}/api/proposals", port))
        .send()
        .await
        .expect("Failed to send request");
    assert_eq!(resp.status(), 200);
    let proposals: Vec<serde_json::Value> = resp.json().await.expect("Failed to parse JSON");
    assert!(
        !proposals.is_empty(),
        "Need at least one proposal for context test"
    );

    let proposal_id = proposals[0]["id"].as_str().unwrap();

    // GET /api/context/:target_id
    let resp = client
        .get(format!(
            "http://127.0.0.1:{}/api/context/{}",
            port, proposal_id
        ))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(
        resp.status(),
        200,
        "context_for should return 200 for a known proposal"
    );
    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    assert_eq!(body["target_id"].as_str().unwrap(), proposal_id);
    assert!(body.get("target_name").is_some(), "Missing target_name");
    assert!(body.get("target_kind").is_some(), "Missing target_kind");
    assert!(body.get("related_seams").is_some(), "Missing related_seams");
    assert!(
        body.get("implementing_code").is_some(),
        "Missing implementing_code"
    );
    assert!(body.get("decisions").is_some(), "Missing decisions");
    assert!(
        body.get("active_sessions").is_some(),
        "Missing active_sessions"
    );
}

#[tokio::test]
async fn test_api_context_for_nonexistent() {
    let (port, client) = start_test_server().await;

    let resp = client
        .get(format!(
            "http://127.0.0.1:{}/api/context/nonexistent:fake:id",
            port
        ))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(
        resp.status(),
        500,
        "context_for should error for nonexistent node"
    );
}

#[tokio::test]
async fn test_api_impact() {
    let (port, client) = start_test_server().await;

    // Find a real proposal to test impact on
    let resp = client
        .get(format!("http://127.0.0.1:{}/api/proposals", port))
        .send()
        .await
        .expect("Failed to send request");
    let proposals: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(
        !proposals.is_empty(),
        "Need at least one proposal for impact test"
    );

    let proposal_id = proposals[0]["id"].as_str().unwrap();

    let resp = client
        .get(format!(
            "http://127.0.0.1:{}/api/impact/{}",
            port, proposal_id
        ))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    assert!(
        body.get("target").is_some(),
        "Missing target in impact response"
    );
    assert!(
        body.get("affected_proposals").is_some(),
        "Missing affected_proposals"
    );
    assert!(
        body.get("affected_seams").is_some(),
        "Missing affected_seams"
    );
    assert!(
        body.get("affected_tests").is_some(),
        "Missing affected_tests"
    );
    assert!(body.get("total_reached").is_some(), "Missing total_reached");
}

#[tokio::test]
async fn test_api_session_start() {
    let (port, client) = start_test_server().await;

    let resp = client
        .post(format!("http://127.0.0.1:{}/api/session/start", port))
        .json(&serde_json::json!({
            "session_id": "test-session-001",
            "agent": "test-agent",
            "agent_model": "test-model",
            "seam_id": "seam:test-seam",
            "proposal_id": "proposal:test-proposal",
            "phase": "testing"
        }))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(resp.status(), 200, "session/start should return 200");
    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    assert_eq!(body["session_id"].as_str().unwrap(), "test-session-001");
    assert_eq!(body["status"].as_str().unwrap(), "active");
    assert!(body.get("workstream_id").is_some(), "Missing workstream_id");
}

#[tokio::test]
async fn test_api_session_close() {
    let (port, client) = start_test_server().await;

    // First start a session
    let resp = client
        .post(format!("http://127.0.0.1:{}/api/session/start", port))
        .json(&serde_json::json!({
            "session_id": "test-session-close-001",
            "agent": "test-agent",
            "seam_id": "seam:test-seam",
        }))
        .send()
        .await
        .expect("Failed to send request");
    assert_eq!(resp.status(), 200);

    // Now close it
    let resp = client
        .post(format!("http://127.0.0.1:{}/api/session/close", port))
        .json(&serde_json::json!({
            "session_id": "test-session-close-001",
            "summary": "Test session completed successfully",
            "files_touched": ["test_file.rs"]
        }))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(resp.status(), 200, "session/close should return 200");
    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    assert_eq!(
        body["session_id"].as_str().unwrap(),
        "test-session-close-001"
    );
    assert_eq!(body["status"].as_str().unwrap(), "closed");
    assert_eq!(
        body["workstream_ids"][0].as_str().unwrap(),
        "workstream:test-seam"
    );

    let resp = client
        .get(format!(
            "http://127.0.0.1:{}/api/nodes?kind=workstream",
            port
        ))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    let workstreams = body.as_array().unwrap();
    let workstream = workstreams
        .iter()
        .find(|node| node["id"].as_str() == Some("workstream:test-seam"))
        .expect("Expected linked workstream node");

    assert_eq!(
        workstream["properties"]["status"].as_str().unwrap(),
        "closed"
    );
    assert_eq!(
        workstream["properties"]["closed_by_session"]
            .as_str()
            .unwrap(),
        "test-session-close-001"
    );
    assert_eq!(
        workstream["properties"]["summary"].as_str().unwrap(),
        "Test session completed successfully"
    );
}

#[tokio::test]
async fn test_api_session_close_nonexistent() {
    let (port, client) = start_test_server().await;

    let resp = client
        .post(format!("http://127.0.0.1:{}/api/session/close", port))
        .json(&serde_json::json!({
            "session_id": "nonexistent-session-999",
        }))
        .send()
        .await
        .expect("Failed to send request");

    // Should fail because the session doesn't exist
    assert_ne!(
        resp.status(),
        200,
        "Closing a nonexistent session should fail"
    );
}

#[tokio::test]
async fn test_api_session_cleanup_closes_workstream() {
    let (port, client) = start_test_server().await;

    let resp = client
        .post(format!("http://127.0.0.1:{}/api/session/start", port))
        .json(&serde_json::json!({
            "session_id": "test-session-cleanup-001",
            "agent": "test-agent",
            "seam_id": "seam:test-cleanup-seam"
        }))
        .send()
        .await
        .expect("Failed to send request");
    assert_eq!(resp.status(), 200);

    let resp = client
        .post(format!("http://127.0.0.1:{}/api/session/cleanup", port))
        .json(&serde_json::json!({
            "max_age_hours": 0
        }))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(resp.status(), 200, "session/cleanup should return 200");
    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    assert_eq!(body["cleaned"].as_u64().unwrap(), 1);

    let resp = client
        .get(format!(
            "http://127.0.0.1:{}/api/nodes?kind=workstream",
            port
        ))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    let workstreams = body.as_array().unwrap();
    let workstream = workstreams
        .iter()
        .find(|node| node["id"].as_str() == Some("workstream:test-cleanup-seam"))
        .expect("Expected linked workstream node");

    assert_eq!(
        workstream["properties"]["status"].as_str().unwrap(),
        "timed_out"
    );
    assert_eq!(
        workstream["properties"]["closed_by_session"]
            .as_str()
            .unwrap(),
        "test-session-cleanup-001"
    );
}

#[tokio::test]
async fn test_api_session_cleanup_reconciles_orphaned_workstream() {
    let (port, client) = start_test_server().await;

    let resp = client
        .post(format!("http://127.0.0.1:{}/api/session/start", port))
        .json(&serde_json::json!({
            "session_id": "orphan-session-001",
            "agent": "test-agent",
            "seam_id": "seam:orphan-seam"
        }))
        .send()
        .await
        .expect("Failed to send request");
    assert_eq!(resp.status(), 200);

    let resp = client
        .post(format!(
            "http://127.0.0.1:{}/api/nodes/orphan-session-001/update",
            port
        ))
        .json(&serde_json::json!({
            "properties": {
                "status": "closed"
            },
            "agent": "test-agent",
            "reason": "Create orphaned workstream fixture"
        }))
        .send()
        .await
        .expect("Failed to send request");
    assert_eq!(resp.status(), 200);

    let resp = client
        .post(format!("http://127.0.0.1:{}/api/session/cleanup", port))
        .json(&serde_json::json!({
            "max_age_hours": 24
        }))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(resp.status(), 200, "session/cleanup should return 200");
    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    assert_eq!(body["orphaned_workstreams_cleaned"].as_u64().unwrap(), 1);
    assert_eq!(
        body["orphaned_workstream_ids"][0].as_str().unwrap(),
        "workstream:orphan-seam"
    );

    let resp = client
        .get(format!(
            "http://127.0.0.1:{}/api/nodes?kind=workstream",
            port
        ))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    let workstreams = body.as_array().unwrap();
    let workstream = workstreams
        .iter()
        .find(|node| node["id"].as_str() == Some("workstream:orphan-seam"))
        .expect("Expected orphaned workstream node");

    assert_eq!(
        workstream["properties"]["status"].as_str().unwrap(),
        "orphaned"
    );
    assert_eq!(
        workstream["properties"]["close_reason"].as_str().unwrap(),
        "auto_cleanup: no active session"
    );
}

#[tokio::test]
async fn test_api_decide() {
    let (port, client) = start_test_server().await;

    let resp = client
        .post(format!("http://127.0.0.1:{}/api/decide", port))
        .json(&serde_json::json!({
            "target_node": "proposal:test-proposal",
            "action": "status_change",
            "from_value": "proposed",
            "to_value": "accepted for current slice",
            "reason": "Test decision for verification",
            "agent": "test-agent",
            "session": "test-session-decide-001"
        }))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(resp.status(), 200, "decide should return 200");
    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    assert!(
        body["decision_id"]
            .as_str()
            .unwrap()
            .starts_with("decision:"),
        "decision_id should start with 'decision:'"
    );
    assert_eq!(
        body["target_node"].as_str().unwrap(),
        "proposal:test-proposal"
    );
    assert_eq!(body["action"].as_str().unwrap(), "status_change");
    assert_eq!(body["status"].as_str().unwrap(), "recorded");
}

/// Memory Transparency Slice M1 (`MEMORY_TRANSPARENCY_PROPOSAL.md`):
/// proof-of-adoption for the intel-graph decision plane — `evidence`,
/// `reversal`, and `trust` (the provenance envelope fields this slice adds
/// to `DecideBody`) must land on both the stored decision node's
/// `properties` and the recorded mutation's `details`, matching what the A3
/// heal-dispatcher / M4 memory-hygiene callers now send.
#[tokio::test]
async fn test_api_decide_carries_provenance_envelope_fields() {
    let (port, client) = start_test_server().await;

    let resp = client
        .post(format!("http://127.0.0.1:{}/api/decide", port))
        .json(&serde_json::json!({
            "target_node": "doc:MEMORY_TRANSPARENCY_PROPOSAL",
            "action": "memory_hygiene_finding_filed",
            "to_value": "memory_hygiene:hotel-1:123",
            "reason": "1 contradiction pair(s) flagged for review",
            "agent": "memory-hygiene-sweep",
            "evidence": ["engram:self_agent-1:eng-a", "engram:self_agent-1:eng-b"],
            "reversal": "review flagged engrams via muninn_contradictions / muninn_consolidate",
            "trust": "observed"
        }))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(resp.status(), 200, "decide should return 200");
    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    let decision_id = body["decision_id"].as_str().unwrap().to_string();

    // The stored decision node's properties carry the envelope fields.
    let resp = client
        .get(format!(
            "http://127.0.0.1:{}/api/nodes/{}",
            port, decision_id
        ))
        .send()
        .await
        .expect("Failed to fetch decision node");
    assert_eq!(resp.status(), 200);
    let node: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    let evidence = node["properties"]["evidence"]
        .as_array()
        .expect("evidence array on decision node");
    assert_eq!(evidence.len(), 2);
    assert_eq!(evidence[0].as_str().unwrap(), "engram:self_agent-1:eng-a");
    assert!(node["properties"]["reversal"]
        .as_str()
        .unwrap()
        .contains("muninn_contradictions"));
    assert_eq!(node["properties"]["trust"].as_str().unwrap(), "observed");

    // The recorded mutation's details carry the same fields.
    let resp = client
        .get(format!("http://127.0.0.1:{}/api/mutations", port))
        .send()
        .await
        .expect("Failed to fetch mutations");
    assert_eq!(resp.status(), 200);
    let mutations: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    let mutation = mutations
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["action"].as_str() == Some("memory_hygiene_finding_filed"))
        .expect("mutation for this decision should be recorded");
    assert_eq!(mutation["details"]["evidence"].as_array().unwrap().len(), 2);
    assert_eq!(mutation["details"]["trust"].as_str().unwrap(), "observed");
}

#[tokio::test]
async fn test_api_full_agent_lifecycle() {
    let (port, client) = start_test_server().await;

    // 1. Start a session
    let resp = client
        .post(format!("http://127.0.0.1:{}/api/session/start", port))
        .json(&serde_json::json!({
            "session_id": "lifecycle-session-001",
            "agent": "lifecycle-agent",
            "seam_id": "seam:lifecycle-seam",
            "phase": "implementation"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 2. Dashboard should show the active session
    let resp = client
        .get(format!("http://127.0.0.1:{}/api/dashboard", port))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let dashboard: serde_json::Value = resp.json().await.unwrap();
    let active = dashboard["active_sessions"].as_array().unwrap();
    let found = active
        .iter()
        .any(|s| s["id"].as_str() == Some("lifecycle-session-001"));
    assert!(
        found,
        "Dashboard should show our active session. Active sessions: {:?}",
        active
    );

    // 3. Record a decision
    let resp = client
        .post(format!("http://127.0.0.1:{}/api/decide", port))
        .json(&serde_json::json!({
            "target_node": "seam:lifecycle-seam",
            "action": "implementation_complete",
            "reason": "Lifecycle test: marking seam as complete",
            "agent": "lifecycle-agent",
            "session": "lifecycle-session-001"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let decide_body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(decide_body["status"].as_str().unwrap(), "recorded");

    // 4. Close the session
    let resp = client
        .post(format!("http://127.0.0.1:{}/api/session/close", port))
        .json(&serde_json::json!({
            "session_id": "lifecycle-session-001",
            "summary": "Lifecycle test completed",
            "files_touched": ["api.rs", "egress.rs"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 5. Dashboard should show session as recently closed, not active
    let resp = client
        .get(format!("http://127.0.0.1:{}/api/dashboard", port))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let dashboard: serde_json::Value = resp.json().await.unwrap();
    let active = dashboard["active_sessions"].as_array().unwrap();
    let still_active = active
        .iter()
        .any(|s| s["id"].as_str() == Some("lifecycle-session-001"));
    assert!(
        !still_active,
        "Closed session should not appear in active_sessions"
    );
}
