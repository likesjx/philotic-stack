//! Live verification of the `ariel-table-cutover` path against the real macOS
//! iMessage store. Ignored by default — it needs a real `~/Library/Messages/chat.db`
//! and Full Disk Access for the test runner.
//!
//! Run with:
//!   cargo test -p table-datasource --test imessage_live -- --ignored --nocapture
//!
//! These tests assert on shape, counts, and access decisions only. They never
//! print message bodies or handles — the point is to prove the governed surface
//! works, not to move the operator's private data into a log.

use datasource::controller::{DatasourceProvider, DatasourceTask, ProviderOutput, TaskKind};
use serde_json::{Value, json};
use table_datasource::SqliteTableProvider;
use tempfile::TempDir;

const ARIEL: &str = "agent-ariel";
const UNGRANTED: &str = "agent-jane";

fn task(
    kind: &str,
    db: Option<&str>,
    query: Option<&str>,
    params: Value,
    agent: &str,
) -> DatasourceTask {
    DatasourceTask {
        kind: TaskKind::Custom(kind.to_string()),
        provider: Some("table".to_string()),
        db: db.map(str::to_string),
        graph_id: None,
        query: query.map(str::to_string),
        parameters: params,
        identity: json!({ "agent_id": agent }),
    }
}

/// Build the provider over a temp profile whose catalog mirrors the exact
/// entry the Ariel cutover will install on the live hotel.
fn provider_with_imessage() -> (SqliteTableProvider, TempDir) {
    let dir = TempDir::new().unwrap();
    let catalog = json!({
        "databases": [{
            "name": "imessage",
            "path": "~/Library/Messages/chat.db",
            "mode": "ro",
            "description": "macOS iMessage store (read-only, snapshot-on-read)",
            "snapshot_on_read": true,
            "snapshot_ttl_secs": 300,
            "agents": { ARIEL: ["read"] }
        }]
    });
    std::fs::write(
        dir.path().join("db_catalog.json"),
        serde_json::to_string_pretty(&catalog).unwrap(),
    )
    .unwrap();
    let p = SqliteTableProvider::new(dir.path()).unwrap();
    (p, dir)
}

fn chat_db_present() -> bool {
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::Path::new(&home)
        .join("Library/Messages/chat.db")
        .exists()
}

#[tokio::test]
#[ignore = "requires a real ~/Library/Messages/chat.db and Full Disk Access"]
async fn ariel_reads_imessage_through_the_governed_surface() {
    if !chat_db_present() {
        eprintln!("skipping: no chat.db on this host");
        return;
    }
    let (p, _d) = provider_with_imessage();

    // 1. Discovery: the catalog advertises the database and Ariel's verbs.
    let ProviderOutput::ResultSet(cat) = p
        .invoke(&task("table.catalog", None, None, json!({}), ARIEL))
        .await
        .unwrap()
    else {
        panic!("expected ResultSet")
    };
    let imessage = cat["databases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["db"] == "imessage")
        .expect("imessage must be visible to Ariel");
    assert_eq!(imessage["mode"], "ro");
    assert_eq!(imessage["verbs"], json!(["read"]));
    assert_eq!(imessage["snapshot_on_read"], true);

    // 2. Schema discovery without prior knowledge of the schema.
    let ProviderOutput::ResultSet(schema) = p
        .invoke(&task(
            "table.schema",
            Some("imessage"),
            None,
            json!({}),
            ARIEL,
        ))
        .await
        .unwrap()
    else {
        panic!()
    };
    let tables: Vec<&str> = schema["tables"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["table"].as_str())
        .collect();
    for expected in ["message", "handle", "chat"] {
        assert!(
            tables.contains(&expected),
            "chat.db should expose {expected}"
        );
    }
    eprintln!("schema discovery: {} tables", tables.len());

    // 3. table.build: recent messages, no SQL written by the agent.
    let ProviderOutput::ResultSet(built) = p
        .invoke(&task(
            "table.build",
            Some("imessage"),
            None,
            json!({
                "table": {"table": "message", "as": "m"},
                "columns": ["m.ROWID", "m.date", "m.is_from_me"],
                "where": [{"column": "m.is_from_me", "op": "=", "value": 0}],
                "order_by": [{"column": "m.date", "dir": "desc"}],
                "limit": 5
            }),
            ARIEL,
        ))
        .await
        .unwrap()
    else {
        panic!()
    };
    let rows = built["rows"].as_array().unwrap();
    assert!(!rows.is_empty(), "expected at least one received message");
    assert!(rows.len() <= 5);
    assert!(rows[0].get("date").is_some());
    eprintln!(
        "table.build compiled: {}\n  -> {} rows",
        built["sql"].as_str().unwrap(),
        rows.len()
    );

    // 4. table.query power lane: the Apple-epoch + handle join a builder can't express.
    let ProviderOutput::ResultSet(Value::Array(joined)) = p
        .invoke(&task(
            "table.query",
            Some("imessage"),
            Some(
                "SELECT datetime(m.date/1000000000 + 978307200, 'unixepoch', 'localtime') AS sent_at, \
                 length(m.text) AS text_len \
                 FROM message m JOIN handle h ON m.handle_id = h.ROWID \
                 WHERE m.text IS NOT NULL ORDER BY m.date DESC",
            ),
            json!({"limit": 3}),
            ARIEL,
        ))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(!joined.is_empty(), "expected joined rows");
    assert!(joined[0]["sent_at"].as_str().unwrap().starts_with("20"));
    eprintln!(
        "table.query join: {} rows, latest {}",
        joined.len(),
        joined[0]["sent_at"]
    );
}

#[tokio::test]
#[ignore = "requires a real ~/Library/Messages/chat.db and Full Disk Access"]
async fn imessage_is_not_writable_and_not_open_to_others() {
    if !chat_db_present() {
        eprintln!("skipping: no chat.db on this host");
        return;
    }
    let (p, _d) = provider_with_imessage();

    // Write task kinds refused outright.
    let err = p
        .invoke(&DatasourceTask {
            kind: TaskKind::Custom("table.delete".into()),
            provider: Some("table".into()),
            db: Some("imessage".into()),
            graph_id: Some("message".into()),
            query: None,
            parameters: json!([]),
            identity: json!({"agent_id": ARIEL}),
        })
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("read-only"), "got: {err}");

    // Writes smuggled through the read lane are refused. Which layer refuses
    // varies by statement and that is fine — what must hold is that none of
    // them execute. (`DELETE FROM message` is rejected at prepare time by
    // chat.db's own triggers, which call Apple-private SQLite functions absent
    // from our bundled build; `DELETE FROM handle` reaches the readonly guard.
    // The guard itself is proven load-bearing against a *writable* database in
    // provider_tests::read_lane_refuses_writes_on_a_writable_database.)
    let baseline: i64 = {
        let ProviderOutput::ResultSet(Value::Array(rows)) = p
            .invoke(&task(
                "table.query",
                Some("imessage"),
                Some("SELECT COUNT(*) AS n FROM handle"),
                json!({}),
                ARIEL,
            ))
            .await
            .unwrap()
        else {
            panic!()
        };
        rows[0]["n"].as_i64().unwrap()
    };

    for sql in [
        "DELETE FROM message WHERE ROWID = -1",
        "DELETE FROM handle WHERE ROWID = -1",
        "UPDATE handle SET id = 'clobbered'",
        "DROP TABLE handle",
    ] {
        let result = p
            .invoke(&task(
                "table.query",
                Some("imessage"),
                Some(sql),
                json!({}),
                ARIEL,
            ))
            .await;
        assert!(result.is_err(), "{sql} must be refused");
    }

    let ProviderOutput::ResultSet(Value::Array(rows)) = p
        .invoke(&task(
            "table.query",
            Some("imessage"),
            Some("SELECT COUNT(*) AS n FROM handle"),
            json!({}),
            ARIEL,
        ))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        rows[0]["n"].as_i64().unwrap(),
        baseline,
        "handle table must be unchanged after refused writes"
    );

    // A different agent has no grant at all.
    let err = p
        .invoke(&task(
            "table.query",
            Some("imessage"),
            Some("SELECT COUNT(*) FROM message"),
            json!({}),
            UNGRANTED,
        ))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no Read grant"), "got: {err}");

    // ...and cannot even see the database exists.
    let ProviderOutput::ResultSet(cat) = p
        .invoke(&task("table.catalog", None, None, json!({}), UNGRANTED))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(
        !cat["databases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["db"] == "imessage"),
        "ungranted agent must not see the imessage entry"
    );

    // The live store is untouched: the snapshot is what was queried.
    let home = std::env::var("HOME").unwrap();
    let live = std::path::Path::new(&home).join("Library/Messages/chat.db");
    let before = std::fs::metadata(&live).unwrap().len();
    let _ = p
        .invoke(&task(
            "table.query",
            Some("imessage"),
            Some("SELECT COUNT(*) AS n FROM message"),
            json!({}),
            ARIEL,
        ))
        .await
        .unwrap();
    assert_eq!(
        before,
        std::fs::metadata(&live).unwrap().len(),
        "live chat.db must not be modified"
    );
}
