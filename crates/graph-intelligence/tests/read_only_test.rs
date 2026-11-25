//! Read-only MCP endpoint and `graph_changes_since`.
//!
//! These tests build a small graph in a temp file, then serve it the way
//! `--read-only` does: a read-only database connection behind
//! `mcp::read_only_router`. They check that write tools are hidden and
//! refused, that a sweep of every read tool leaves the database file
//! untouched, and that `graph_changes_since` filters by time and agent.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use graph_intelligence::server::ws::ChangeEvent;
use graph_intelligence::server::{mcp, AppState};
use graph_intelligence::{GraphEngine, Mutation, Node, NodeKind, ScanConfig};
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, Mutex};

fn node(id: &str, kind: NodeKind, props: serde_json::Value, at: DateTime<Utc>) -> Node {
    Node {
        id: id.to_string(),
        kind,
        name: id.to_string(),
        properties: props,
        file_path: None,
        worktree: "develop".to_string(),
        created_at: at,
        updated_at: at,
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    }
}

fn mutation(id: &str, agent: &str, target: &str, at: DateTime<Utc>) -> Mutation {
    Mutation {
        id: id.to_string(),
        timestamp: at,
        agent: Some(agent.to_string()),
        session: None,
        action: "update_node".to_string(),
        target_node: Some(target.to_string()),
        from_value: None,
        to_value: None,
        reason: Some(format!("test change {id}")),
        details: serde_json::json!({}),
    }
}

/// Populate `engine` with an old change, and three new ones: a task for
/// claude-local, a task for codex, and a code node a rescan touched.
fn seed(engine: &GraphEngine, base: DateTime<Utc>) {
    let old = base - Duration::hours(2);
    let new = base + Duration::minutes(5);
    engine
        .upsert_node(&node(
            "task:old",
            NodeKind::Task,
            serde_json::json!({"owner": "claude-local", "status": "open"}),
            old,
        ))
        .unwrap();
    engine
        .upsert_node(&node(
            "task:for-claude",
            NodeKind::Task,
            serde_json::json!({"owner": "claude-local", "status": "open"}),
            new,
        ))
        .unwrap();
    engine
        .upsert_node(&node(
            "task:for-codex",
            NodeKind::Task,
            serde_json::json!({"owner": "codex", "status": "open"}),
            new,
        ))
        .unwrap();
    engine
        .upsert_node(&node(
            "function:rescanned",
            NodeKind::Function,
            serde_json::json!({}),
            new,
        ))
        .unwrap();
    engine
        .record_mutation(&mutation("m-old", "codex", "task:old", old))
        .unwrap();
    engine
        .record_mutation(&mutation("m-claude", "priti", "task:for-claude", new))
        .unwrap();
    engine
        .record_mutation(&mutation("m-self", "claude-local", "task:for-claude", new))
        .unwrap();
    engine
        .record_mutation(&mutation("m-codex", "priti", "task:for-codex", new))
        .unwrap();
}

fn state_for(engine: GraphEngine) -> Arc<AppState> {
    let (change_tx, _) = broadcast::channel::<ChangeEvent>(16);
    Arc::new(AppState {
        engine: Mutex::new(engine),
        scan_config: ScanConfig {
            rust_roots: vec![],
            doc_roots: vec![],
            git_repo: ".".to_string(),
            worktree: "develop".to_string(),
        },
        repo_root: ".".to_string(),
        change_tx,
        server_info: serde_json::json!({"version": "test"}),
    })
}

async fn serve(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://127.0.0.1:{port}/mcp")
}

async fn rpc(
    client: &reqwest::Client,
    url: &str,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    client
        .post(url)
        .json(&serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn call(
    client: &reqwest::Client,
    url: &str,
    tool: &str,
    args: serde_json::Value,
) -> serde_json::Value {
    rpc(
        client,
        url,
        "tools/call",
        serde_json::json!({"name": tool, "arguments": args}),
    )
    .await
}

fn tool_text(resp: &serde_json::Value) -> serde_json::Value {
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text result: {resp}"));
    serde_json::from_str(text).unwrap()
}

fn ids(list: &serde_json::Value) -> Vec<String> {
    let mut out: Vec<String> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap().to_string())
        .collect();
    out.sort();
    out
}

fn digest_db(dir: &std::path::Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        // The -shm file is SQLite's shared-memory index; readers may touch it.
        if name.ends_with("-shm") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        out.push((name, format!("{:x}", Sha256::digest(&bytes))));
    }
    out.sort();
    out
}

#[tokio::test]
async fn read_only_endpoint_hides_and_refuses_writes_and_leaves_db_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("graph.db");
    let db = db.to_str().unwrap();
    let base = Utc::now();
    {
        let engine = GraphEngine::open(db).unwrap();
        seed(&engine, base);
    }

    let engine = GraphEngine::open_read_only(db).unwrap();
    // The connection itself refuses writes, whatever the tool layer does.
    assert!(engine
        .upsert_node(&node(
            "task:sneaky",
            NodeKind::Task,
            serde_json::json!({}),
            base
        ))
        .is_err());

    let before = digest_db(dir.path());
    let url = serve(mcp::read_only_router(state_for(engine))).await;
    let client = reqwest::Client::new();

    let listed = rpc(&client, &url, "tools/list", serde_json::json!({})).await;
    let mut names: Vec<String> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    let mut expected: Vec<String> = mcp::READ_ONLY_TOOLS.iter().map(|s| s.to_string()).collect();
    expected.sort();
    assert_eq!(names, expected);

    for write_tool in [
        "graph_create_node",
        "graph_update_node",
        "graph_create_edge",
        "graph_decide",
        "graph_manage_proposal",
        "graph_scan",
        "session_start",
        "session_activity",
        "graph_persist_diagrams",
    ] {
        let resp = call(
            &client,
            &url,
            write_tool,
            serde_json::json!({"id": "task:sneaky", "kind": "task", "name": "x"}),
        )
        .await;
        assert_eq!(resp["error"]["code"], -32601, "{write_tool}: {resp}");
        assert!(resp["result"].is_null(), "{write_tool}: {resp}");
    }

    // Sweep every read tool. Some need arguments they don't get here and
    // answer with an error; what matters is that none of them writes.
    let since = (base - Duration::minutes(1)).to_rfc3339();
    for tool in mcp::READ_ONLY_TOOLS {
        let args = match *tool {
            "graph_changes_since" => serde_json::json!({"since": since}),
            "graph_node" | "graph_context_for" | "graph_impact" => {
                serde_json::json!({"id": "task:for-claude", "proposal_id": "task:for-claude", "node_id": "task:for-claude"})
            }
            "graph_search" => serde_json::json!({"query": "claude"}),
            "graph_skeleton" | "graph_snippet" => serde_json::json!({"crate": "x", "id": "x"}),
            _ => serde_json::json!({}),
        };
        let resp = call(&client, &url, tool, args).await;
        assert!(
            resp.get("result").is_some() || resp.get("error").is_some(),
            "{tool}: {resp}"
        );
    }

    assert_eq!(before, digest_db(dir.path()), "database files changed");
}

#[tokio::test]
async fn changes_since_filters_by_time_kind_and_agent() {
    let base = Utc::now();
    let engine = GraphEngine::open(":memory:").unwrap();
    seed(&engine, base);
    let url = serve(mcp::router(state_for(engine))).await;
    let client = reqwest::Client::new();
    let since = (base - Duration::minutes(1)).to_rfc3339();

    // Default kinds: ledger nodes only, old change left out.
    let all = tool_text(
        &call(
            &client,
            &url,
            "graph_changes_since",
            serde_json::json!({"since": since}),
        )
        .await,
    );
    assert_eq!(
        ids(&all["nodes"]),
        vec!["task:for-claude", "task:for-codex"]
    );
    assert_eq!(
        ids(&all["mutations"]),
        vec!["m-claude", "m-codex", "m-self"]
    );
    assert_eq!(all["truncated"], false);

    // kinds = all brings the rescanned code node back.
    let every = tool_text(
        &call(
            &client,
            &url,
            "graph_changes_since",
            serde_json::json!({"since": since, "kinds": ["all"]}),
        )
        .await,
    );
    assert!(ids(&every["nodes"]).contains(&"function:rescanned".to_string()));

    // Addressed to claude-local, without its own writes.
    let mine = tool_text(
        &call(
            &client,
            &url,
            "graph_changes_since",
            serde_json::json!({"since": since, "for_agent": "claude-local", "exclude_agent": "claude-local"}),
        )
        .await,
    );
    assert_eq!(ids(&mine["nodes"]), vec!["task:for-claude"]);
    assert_eq!(ids(&mine["mutations"]), vec!["m-claude"]);

    // Paging: limit 1 cuts the lists short and hands back a cursor.
    let page = tool_text(
        &call(
            &client,
            &url,
            "graph_changes_since",
            serde_json::json!({"since": since, "limit": 1}),
        )
        .await,
    );
    assert_eq!(page["truncated"], true);
    assert_eq!(page["nodes"].as_array().unwrap().len(), 1);

    // Nothing after the newest change.
    let later = (base + Duration::hours(1)).to_rfc3339();
    let none = tool_text(
        &call(
            &client,
            &url,
            "graph_changes_since",
            serde_json::json!({"since": later}),
        )
        .await,
    );
    assert!(none["nodes"].as_array().unwrap().is_empty());
    assert!(none["mutations"].as_array().unwrap().is_empty());
    assert_eq!(none["next_since"], none["since"]);

    // Bad input is an error, not an empty answer.
    let bad = call(
        &client,
        &url,
        "graph_changes_since",
        serde_json::json!({"since": "yesterday"}),
    )
    .await;
    assert!(bad["error"]["message"]
        .as_str()
        .unwrap()
        .contains("RFC 3339"));
}
