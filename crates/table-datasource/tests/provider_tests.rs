//! Provider behavior: legacy profile databases, the known-database catalog,
//! read-only enforcement, per-agent grants, the query builder, and
//! catalog-gated attach.

use datasource::controller::{DatasourceProvider, DatasourceTask, ProviderOutput, TaskKind};
use serde_json::{Value, json};
use table_datasource::SqliteTableProvider;
use tempfile::TempDir;

fn open_tmp() -> (SqliteTableProvider, TempDir) {
    let d = TempDir::new().unwrap();
    let p = SqliteTableProvider::new(d.path()).unwrap();
    (p, d)
}

fn make_task(
    kind: &str,
    db: Option<&str>,
    graph_id: Option<&str>,
    query: Option<&str>,
    params: Value,
) -> DatasourceTask {
    make_task_as(kind, db, graph_id, query, params, "codex")
}

fn make_task_as(
    kind: &str,
    db: Option<&str>,
    graph_id: Option<&str>,
    query: Option<&str>,
    params: Value,
    agent: &str,
) -> DatasourceTask {
    DatasourceTask {
        kind: TaskKind::Custom(kind.to_string()),
        provider: Some("table".to_string()),
        db: db.map(str::to_string),
        graph_id: graph_id.map(str::to_string),
        query: query.map(str::to_string),
        parameters: params,
        identity: json!({"agent_id": agent}),
    }
}

async fn seed_signals(p: &SqliteTableProvider, db: Option<&str>) {
    p.invoke(&make_task(
        "table.configure",
        db,
        None,
        Some("CREATE TABLE IF NOT EXISTS signals (id INTEGER PRIMARY KEY AUTOINCREMENT, provider TEXT NOT NULL, latency_ms INTEGER, timestamp INTEGER NOT NULL)"),
        json!({}),
    )).await.unwrap();
    for (prov, lat, ts) in [("gemini", 120, 1_000_000), ("ollama", 45, 1_000_001)] {
        p.invoke(&make_task(
            "table.insert",
            db,
            Some("signals"),
            None,
            json!({"provider": prov, "latency_ms": lat, "timestamp": ts}),
        ))
        .await
        .unwrap();
    }
}

/// Write a catalog file into the provider's base dir and return a fresh
/// provider that reads it (the constructor loads the catalog).
fn write_catalog(dir: &TempDir, databases: Value) -> SqliteTableProvider {
    std::fs::write(
        dir.path().join("db_catalog.json"),
        serde_json::to_string_pretty(&json!({ "databases": databases })).unwrap(),
    )
    .unwrap();
    SqliteTableProvider::new(dir.path()).unwrap()
}

// ── legacy behavior (pre-catalog tests, kept green) ──────────────────────────

#[tokio::test]
async fn configure_and_insert_and_query() {
    let (p, _d) = open_tmp();
    seed_signals(&p, None).await;

    let out = p
        .invoke(&make_task(
            "table.query",
            None,
            None,
            Some("SELECT provider, latency_ms FROM signals ORDER BY timestamp ASC"),
            json!({}),
        ))
        .await
        .unwrap();

    let ProviderOutput::ResultSet(Value::Array(rows)) = out else {
        panic!("expected ResultSet")
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["provider"], "gemini");
    assert_eq!(rows[1]["provider"], "ollama");
}

#[tokio::test]
async fn upsert_replaces_existing() {
    let (p, _d) = open_tmp();
    p.invoke(&make_task(
        "table.exec",
        None,
        None,
        Some("CREATE TABLE IF NOT EXISTS kv (id TEXT PRIMARY KEY, val TEXT)"),
        json!({}),
    ))
    .await
    .unwrap();
    for val in ["first", "second"] {
        p.invoke(&make_task(
            "table.upsert",
            None,
            Some("kv"),
            None,
            json!({"id": "k1", "val": val}),
        ))
        .await
        .unwrap();
    }
    let out = p
        .invoke(&make_task(
            "table.query",
            None,
            None,
            Some("SELECT val FROM kv WHERE id = 'k1'"),
            json!({}),
        ))
        .await
        .unwrap();
    let ProviderOutput::ResultSet(Value::Array(rows)) = out else {
        panic!()
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["val"], "second");
}

#[tokio::test]
async fn update_with_where() {
    let (p, _d) = open_tmp();
    p.invoke(&make_task(
        "table.exec",
        None,
        None,
        Some("CREATE TABLE IF NOT EXISTS items (id TEXT PRIMARY KEY, status INTEGER DEFAULT 0)"),
        json!({}),
    ))
    .await
    .unwrap();
    for id in ["a", "b", "c"] {
        p.invoke(&make_task(
            "table.insert",
            None,
            Some("items"),
            None,
            json!({"id": id, "status": 0}),
        ))
        .await
        .unwrap();
    }
    let out = p
        .invoke(&make_task(
            "table.update",
            None,
            Some("items"),
            None,
            json!({"set": {"status": 1}, "where": "id = ?1", "params": ["b"]}),
        ))
        .await
        .unwrap();
    let ProviderOutput::ResultSet(r) = out else {
        panic!()
    };
    assert_eq!(r["rows_affected"], 1);
}

#[tokio::test]
async fn delete_with_where() {
    let (p, _d) = open_tmp();
    p.invoke(&make_task(
        "table.exec",
        None,
        None,
        Some("CREATE TABLE IF NOT EXISTS items (id TEXT PRIMARY KEY)"),
        json!({}),
    ))
    .await
    .unwrap();
    for id in ["x", "y", "z"] {
        p.invoke(&make_task(
            "table.insert",
            None,
            Some("items"),
            None,
            json!({"id": id}),
        ))
        .await
        .unwrap();
    }
    let out = p
        .invoke(&make_task(
            "table.delete",
            None,
            Some("items"),
            Some("id = ?1"),
            json!(["y"]),
        ))
        .await
        .unwrap();
    let ProviderOutput::ResultSet(r) = out else {
        panic!()
    };
    assert_eq!(r["rows_affected"], 1);
}

#[tokio::test]
async fn multi_db_isolation() {
    let (p, _d) = open_tmp();
    for db in ["db_alpha", "db_beta"] {
        p.invoke(&make_task(
            "table.exec",
            Some(db),
            None,
            Some("CREATE TABLE IF NOT EXISTS t (val TEXT)"),
            json!({}),
        ))
        .await
        .unwrap();
    }
    p.invoke(&make_task(
        "table.insert",
        Some("db_alpha"),
        Some("t"),
        None,
        json!({"val": "alpha_row"}),
    ))
    .await
    .unwrap();
    let out = p
        .invoke(&make_task(
            "table.query",
            Some("db_beta"),
            None,
            Some("SELECT COUNT(*) as n FROM t"),
            json!({}),
        ))
        .await
        .unwrap();
    let ProviderOutput::ResultSet(Value::Array(rows)) = out else {
        panic!()
    };
    assert_eq!(rows[0]["n"], 0, "db_beta must be empty");
}

#[tokio::test]
async fn rolloff_by_max_rows() {
    let (p, _d) = open_tmp();
    p.invoke(&make_task(
        "table.configure",
        None,
        None,
        Some("CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts INTEGER NOT NULL, val TEXT)"),
        json!({}),
    )).await.unwrap();
    for i in 0..10u64 {
        p.invoke(&make_task(
            "table.insert",
            None,
            Some("events"),
            None,
            json!({"ts": i, "val": format!("row-{i}")}),
        ))
        .await
        .unwrap();
    }
    p.invoke(&make_task(
        "table.rolloff",
        None,
        Some("events"),
        None,
        json!({"max_rows": 5, "ts_column": "ts"}),
    ))
    .await
    .unwrap();
    let out = p
        .invoke(&make_task(
            "table.stats",
            None,
            Some("events"),
            None,
            json!({"ts_column": "ts"}),
        ))
        .await
        .unwrap();
    let ProviderOutput::ResultSet(stats) = out else {
        panic!()
    };
    assert_eq!(stats["row_count"], 5);
}

#[tokio::test]
async fn schema_returns_ddl() {
    let (p, _d) = open_tmp();
    p.invoke(&make_task(
        "table.configure",
        None,
        None,
        Some("CREATE TABLE IF NOT EXISTS meta (id TEXT PRIMARY KEY, val TEXT)"),
        json!({}),
    ))
    .await
    .unwrap();
    let out = p
        .invoke(&make_task(
            "table.schema",
            None,
            Some("meta"),
            None,
            json!({}),
        ))
        .await
        .unwrap();
    let ProviderOutput::ResultSet(s) = out else {
        panic!()
    };
    assert!(s["schema_sql"].as_str().unwrap().contains("CREATE TABLE"));
}

#[tokio::test]
async fn rejects_invalid_identifier() {
    let (p, _d) = open_tmp();
    let result = p
        .invoke(&make_task(
            "table.rolloff",
            None,
            Some("bad\"table"),
            None,
            json!({"max_rows": 1}),
        ))
        .await;
    assert!(result.is_err());
}

// ── seam A: catalog + traversal fix ──────────────────────────────────────────

#[tokio::test]
async fn db_name_path_traversal_rejected() {
    let (p, _d) = open_tmp();
    let result = p
        .invoke(&make_task(
            "table.query",
            Some("../../evil"),
            None,
            Some("SELECT 1"),
            json!({}),
        ))
        .await;
    let err = result.unwrap_err().to_string();
    assert!(err.contains("unknown database"), "got: {err}");
}

#[tokio::test]
async fn catalog_readonly_blocks_write_kinds() {
    let ext = TempDir::new().unwrap();
    let ext_provider = SqliteTableProvider::new(ext.path()).unwrap();
    seed_signals(&ext_provider, Some("messages")).await;

    let dir = TempDir::new().unwrap();
    let p = write_catalog(
        &dir,
        json!([{
            "name": "messages",
            "path": ext.path().join("messages.db").to_str().unwrap(),
            "mode": "ro",
            "description": "external message store"
        }]),
    );

    // Reads work.
    let out = p
        .invoke(&make_task(
            "table.query",
            Some("messages"),
            None,
            Some("SELECT COUNT(*) AS n FROM signals"),
            json!({}),
        ))
        .await
        .unwrap();
    let ProviderOutput::ResultSet(Value::Array(rows)) = out else {
        panic!()
    };
    assert_eq!(rows[0]["n"], 2);

    // Write task kinds are refused before touching the connection.
    let err = p
        .invoke(&make_task(
            "table.insert",
            Some("messages"),
            Some("signals"),
            None,
            json!({"provider": "evil", "timestamp": 1}),
        ))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("read-only"), "got: {err}");

    // Writes smuggled through the read lane are refused by the statement check.
    let err = p
        .invoke(&make_task(
            "table.query",
            Some("messages"),
            None,
            Some("DELETE FROM signals"),
            json!({}),
        ))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("read-only"), "got: {err}");
}

#[tokio::test]
async fn catalog_unknown_database_rejected_even_with_valid_name() {
    let dir = TempDir::new().unwrap();
    let p = write_catalog(
        &dir,
        json!([{
            "name": "known",
            "path": "/nonexistent/known.db",
            "mode": "ro"
        }]),
    );
    // A catalog entry whose file is missing errors instead of auto-creating.
    let err = p
        .invoke(&make_task(
            "table.query",
            Some("known"),
            None,
            Some("SELECT 1"),
            json!({}),
        ))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("missing"), "got: {err}");
}

// ── seam B: authorizer + read lane ───────────────────────────────────────────

#[tokio::test]
async fn attach_via_agent_sql_denied() {
    let (p, d) = open_tmp();
    seed_signals(&p, None).await;
    let side = d.path().join("side.db");
    let err = p
        .invoke(&make_task(
            "table.query",
            None,
            None,
            Some(&format!("ATTACH DATABASE '{}' AS side", side.display())),
            json!({}),
        ))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("not authorized") || err.contains("read-only"),
        "got: {err}"
    );
}

// ── seam C: query builder + cross-db attach ──────────────────────────────────

#[tokio::test]
async fn build_where_order_limit() {
    let (p, _d) = open_tmp();
    seed_signals(&p, None).await;

    let out = p
        .invoke(&make_task(
            "table.build",
            None,
            None,
            None,
            json!({
                "table": "signals",
                "columns": ["provider", "latency_ms"],
                "where": [{"column": "latency_ms", "op": ">", "value": 50}],
                "order_by": [{"column": "latency_ms", "dir": "desc"}],
                "limit": 10
            }),
        ))
        .await
        .unwrap();
    let ProviderOutput::ResultSet(result) = out else {
        panic!()
    };
    assert!(result["sql"].as_str().unwrap().starts_with("SELECT"));
    let rows = result["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["provider"], "gemini");
}

#[tokio::test]
async fn build_cross_db_join_attaches_catalog_db() {
    // Primary profile DB holds messages; a second catalog DB holds contacts.
    let ext = TempDir::new().unwrap();
    let ext_provider = SqliteTableProvider::new(ext.path()).unwrap();
    ext_provider
        .invoke(&make_task(
            "table.exec",
            Some("contacts"),
            None,
            Some("CREATE TABLE handle (rowid_alias INTEGER, handle_id TEXT)"),
            json!({}),
        ))
        .await
        .unwrap();
    ext_provider
        .invoke(&make_task(
            "table.insert",
            Some("contacts"),
            Some("handle"),
            None,
            json!({"rowid_alias": 1, "handle_id": "+15551234567"}),
        ))
        .await
        .unwrap();

    let dir = TempDir::new().unwrap();
    let p = write_catalog(
        &dir,
        json!([{
            "name": "contacts",
            "path": ext.path().join("contacts.db").to_str().unwrap(),
            "mode": "ro"
        }]),
    );
    p.invoke(&make_task(
        "table.exec",
        None,
        None,
        Some("CREATE TABLE msg (text TEXT, handle_ref INTEGER)"),
        json!({}),
    ))
    .await
    .unwrap();
    p.invoke(&make_task(
        "table.insert",
        None,
        Some("msg"),
        None,
        json!({"text": "hello", "handle_ref": 1}),
    ))
    .await
    .unwrap();

    let out = p
        .invoke(&make_task(
            "table.build",
            None,
            None,
            None,
            json!({
                "table": {"table": "msg", "as": "m"},
                "columns": ["m.text", "h.handle_id"],
                "joins": [{"db": "contacts", "table": "handle", "as": "h",
                            "on": {"left": "m.handle_ref", "right": "h.rowid_alias"}}]
            }),
        ))
        .await
        .unwrap();
    let ProviderOutput::ResultSet(result) = out else {
        panic!()
    };
    let rows = result["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["handle_id"], "+15551234567");
}

// ── seam D: per-agent grants ─────────────────────────────────────────────────

#[tokio::test]
async fn grants_enforced_per_agent() {
    let ext = TempDir::new().unwrap();
    let ext_provider = SqliteTableProvider::new(ext.path()).unwrap();
    seed_signals(&ext_provider, Some("messages")).await;

    let dir = TempDir::new().unwrap();
    let p = write_catalog(
        &dir,
        json!([{
            "name": "messages",
            "path": ext.path().join("messages.db").to_str().unwrap(),
            "mode": "ro",
            "agents": {"agent-ariel": ["read"]}
        }]),
    );

    let sql = "SELECT COUNT(*) AS n FROM signals";

    // Ungranted agent denied.
    let err = p
        .invoke(&make_task_as(
            "table.query",
            Some("messages"),
            None,
            Some(sql),
            json!({}),
            "agent-jane",
        ))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no Read grant"), "got: {err}");

    // Granted agent succeeds.
    let out = p
        .invoke(&make_task_as(
            "table.query",
            Some("messages"),
            None,
            Some(sql),
            json!({}),
            "agent-ariel",
        ))
        .await
        .unwrap();
    let ProviderOutput::ResultSet(Value::Array(rows)) = out else {
        panic!()
    };
    assert_eq!(rows[0]["n"], 2);
}

#[tokio::test]
async fn catalog_tool_reports_grants() {
    let ext = TempDir::new().unwrap();
    let ext_provider = SqliteTableProvider::new(ext.path()).unwrap();
    seed_signals(&ext_provider, Some("messages")).await;

    let dir = TempDir::new().unwrap();
    let p = write_catalog(
        &dir,
        json!([{
            "name": "messages",
            "path": ext.path().join("messages.db").to_str().unwrap(),
            "mode": "ro",
            "description": "operator message store",
            "agents": {"agent-ariel": ["read"]}
        }]),
    );

    let out = p
        .invoke(&make_task_as(
            "table.catalog",
            None,
            None,
            None,
            json!({}),
            "agent-ariel",
        ))
        .await
        .unwrap();
    let ProviderOutput::ResultSet(result) = out else {
        panic!()
    };
    let dbs = result["databases"].as_array().unwrap();
    let messages = dbs.iter().find(|d| d["db"] == "messages").unwrap();
    assert_eq!(messages["mode"], "ro");
    assert_eq!(messages["verbs"], json!(["read"]));

    // An ungranted agent does not even see the entry.
    let out = p
        .invoke(&make_task_as(
            "table.catalog",
            None,
            None,
            None,
            json!({}),
            "agent-jane",
        ))
        .await
        .unwrap();
    let ProviderOutput::ResultSet(result) = out else {
        panic!()
    };
    assert!(
        !result["databases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["db"] == "messages")
    );
}

// ── snapshot-on-read ─────────────────────────────────────────────────────────

#[tokio::test]
async fn snapshot_on_read_serves_stable_copy() {
    let ext = TempDir::new().unwrap();
    let ext_provider = SqliteTableProvider::new(ext.path()).unwrap();
    seed_signals(&ext_provider, Some("live")).await;

    let dir = TempDir::new().unwrap();
    let p = write_catalog(
        &dir,
        json!([{
            "name": "live",
            "path": ext.path().join("live.db").to_str().unwrap(),
            "mode": "ro",
            "snapshot_on_read": true,
            "snapshot_ttl_secs": 3600
        }]),
    );

    let sql = "SELECT COUNT(*) AS n FROM signals";
    let count = |out: ProviderOutput| -> i64 {
        let ProviderOutput::ResultSet(Value::Array(rows)) = out else {
            panic!()
        };
        rows[0]["n"].as_i64().unwrap()
    };

    let first = count(
        p.invoke(&make_task(
            "table.query",
            Some("live"),
            None,
            Some(sql),
            json!({}),
        ))
        .await
        .unwrap(),
    );
    assert_eq!(first, 2);

    // Write into the live source AFTER the snapshot was taken.
    ext_provider
        .invoke(&make_task(
            "table.insert",
            Some("live"),
            Some("signals"),
            None,
            json!({"provider": "late", "latency_ms": 1, "timestamp": 2_000_000}),
        ))
        .await
        .unwrap();

    // Within the TTL the snapshot copy still serves the old row count.
    let second = count(
        p.invoke(&make_task(
            "table.query",
            Some("live"),
            None,
            Some(sql),
            json!({}),
        ))
        .await
        .unwrap(),
    );
    assert_eq!(
        second, 2,
        "snapshot must not see live writes inside the TTL"
    );
}

/// The load-bearing case for the readonly guard: on a READ-WRITE database the
/// connection flags protect nothing, so `stmt.readonly()` is the only thing
/// standing between the read lane and a destructive statement.
#[tokio::test]
async fn read_lane_refuses_writes_on_a_writable_database() {
    let (p, _d) = open_tmp();
    p.invoke(&make_task(
        "table.exec",
        None,
        None,
        Some("CREATE TABLE t (id TEXT PRIMARY KEY)"),
        json!({}),
    ))
    .await
    .unwrap();
    p.invoke(&make_task(
        "table.insert",
        None,
        Some("t"),
        None,
        json!({"id": "keep-me"}),
    ))
    .await
    .unwrap();

    for sql in [
        "DELETE FROM t",
        "UPDATE t SET id = 'clobbered'",
        "INSERT INTO t (id) VALUES ('injected')",
        "DROP TABLE t",
    ] {
        let err = p
            .invoke(&make_task("table.query", None, None, Some(sql), json!({})))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("read-only"), "{sql} -> {err}");
    }

    // The row is untouched and the table still exists.
    let out = p
        .invoke(&make_task(
            "table.query",
            None,
            None,
            Some("SELECT id FROM t"),
            json!({}),
        ))
        .await
        .unwrap();
    let ProviderOutput::ResultSet(Value::Array(rows)) = out else {
        panic!()
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "keep-me");
}

/// Same guard on the builder lane: a spec cannot smuggle a write.
#[tokio::test]
async fn builder_cannot_express_a_write() {
    let (p, _d) = open_tmp();
    p.invoke(&make_task(
        "table.exec",
        None,
        None,
        Some("CREATE TABLE t (id TEXT)"),
        json!({}),
    ))
    .await
    .unwrap();
    // Injection attempts land in the identifier validator, not in SQL.
    for spec in [
        json!({"table": "t; DELETE FROM t"}),
        json!({"table": "t", "columns": ["id) ; DELETE FROM t --"]}),
        json!({"table": "t", "where": [{"column": "id", "op": "=; DELETE FROM t", "value": 1}]}),
    ] {
        assert!(
            p.invoke(&make_task("table.build", None, None, None, spec.clone()))
                .await
                .is_err(),
            "spec should be rejected: {spec}"
        );
    }
}

/// Snapshot-on-read materializes a full copy of the source under
/// `{base_dir}/.snapshots/`. That copy must not become a back door: an
/// ungranted agent must not reach it by any database name it can spell.
#[tokio::test]
async fn ungranted_agent_cannot_reach_a_snapshot() {
    let ext = TempDir::new().unwrap();
    let ext_provider = SqliteTableProvider::new(ext.path()).unwrap();
    seed_signals(&ext_provider, Some("private")).await;

    let dir = TempDir::new().unwrap();
    let p = write_catalog(
        &dir,
        json!([{
            "name": "private",
            "path": ext.path().join("private.db").to_str().unwrap(),
            "mode": "ro",
            "snapshot_on_read": true,
            "snapshot_ttl_secs": 3600,
            "agents": {"agent-ariel": ["read"]}
        }]),
    );

    // Granted read materializes .snapshots/private.db.
    p.invoke(&make_task_as(
        "table.query",
        Some("private"),
        None,
        Some("SELECT COUNT(*) AS n FROM signals"),
        json!({}),
        "agent-ariel",
    ))
    .await
    .unwrap();
    assert!(
        dir.path().join(".snapshots/private.db").exists(),
        "snapshot should exist for the rest of this test to mean anything"
    );

    // No name an ungranted agent can spell reaches it — as the primary db...
    for name in ["private", "snapshots", ".snapshots", ".snapshots/private"] {
        let result = p
            .invoke(&make_task_as(
                "table.query",
                Some(name),
                None,
                Some("SELECT COUNT(*) AS n FROM signals"),
                json!({}),
                "agent-jane",
            ))
            .await;
        assert!(
            result.is_err(),
            "db {name:?} must be denied to an ungranted agent"
        );
    }

    // ...and via the attach lane.
    for name in ["private", ".snapshots/private"] {
        let result = p
            .invoke(&make_task_as(
                "table.query",
                None,
                None,
                Some("SELECT 1"),
                json!({"attach": [name]}),
                "agent-jane",
            ))
            .await;
        assert!(
            result.is_err(),
            "attach {name:?} must be denied to an ungranted agent"
        );
    }

    // The snapshot is also not enumerable as a profile database.
    let ProviderOutput::ResultSet(cat) = p
        .invoke(&make_task_as(
            "table.catalog",
            None,
            None,
            None,
            json!({}),
            "agent-jane",
        ))
        .await
        .unwrap()
    else {
        panic!()
    };
    let names: Vec<&str> = cat["databases"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["db"].as_str())
        .collect();
    assert!(
        !names
            .iter()
            .any(|n| n.contains("private") || n.contains("snapshot")),
        "ungranted agent must not see the snapshot: {names:?}"
    );
}

/// Snapshots are plaintext copies of external databases. They must be
/// owner-only on disk, and removed when the provider shuts down cleanly.
#[cfg(unix)]
#[tokio::test]
async fn snapshots_are_owner_only_and_purged_on_drop() {
    use std::os::unix::fs::PermissionsExt;

    let ext = TempDir::new().unwrap();
    let ext_provider = SqliteTableProvider::new(ext.path()).unwrap();
    seed_signals(&ext_provider, Some("private")).await;

    let dir = TempDir::new().unwrap();
    let snap_dir = dir.path().join(".snapshots");
    let snap = snap_dir.join("private.db");

    {
        let p = write_catalog(
            &dir,
            json!([{
                "name": "private",
                "path": ext.path().join("private.db").to_str().unwrap(),
                "mode": "ro",
                "snapshot_on_read": true,
                "snapshot_ttl_secs": 3600
            }]),
        );
        p.invoke(&make_task(
            "table.query",
            Some("private"),
            None,
            Some("SELECT COUNT(*) AS n FROM signals"),
            json!({}),
        ))
        .await
        .unwrap();

        assert!(snap.exists(), "snapshot should have been taken");
        let file_mode = std::fs::metadata(&snap).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            file_mode, 0o600,
            "snapshot must be owner-only, got {file_mode:o}"
        );
        let dir_mode = std::fs::metadata(&snap_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            dir_mode, 0o700,
            "snapshot dir must be owner-only, got {dir_mode:o}"
        );
    } // provider dropped here

    assert!(
        !snap_dir.exists(),
        "snapshots must not outlive a clean provider shutdown"
    );
}
