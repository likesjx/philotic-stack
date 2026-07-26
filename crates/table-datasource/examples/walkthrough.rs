//! A guided tour of the governed query surface — the same calls an agent makes.
//!
//!   cargo run -p table-datasource --example walkthrough
//!
//! Uses your real `~/Library/Messages/chat.db` when it is readable (needs Full
//! Disk Access), otherwise builds a small synthetic store so the tour runs
//! anywhere. Output is redacted: row counts, column names, text *lengths*, and
//! masked handles — never message bodies.

use datasource::controller::{DatasourceProvider, DatasourceTask, ProviderOutput, TaskKind};
use serde_json::{Value, json};
use table_datasource::SqliteTableProvider;

const ARIEL: &str = "agent-ariel";
const OTHER: &str = "agent-jane";

fn call(
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

fn step(n: u32, title: &str) {
    println!("\n\x1b[1m── {n}. {title}\x1b[0m");
}

fn show(label: &str, v: &Value) {
    let pretty = serde_json::to_string_pretty(v).unwrap_or_default();
    let clipped: String = pretty.lines().take(14).collect::<Vec<_>>().join("\n");
    println!("  {label}:\n{}", indent(&clipped));
}

fn indent(s: &str) -> String {
    s.lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn run(p: &SqliteTableProvider, task: DatasourceTask) -> Result<Value, String> {
    match p.invoke(&task).await {
        Ok(ProviderOutput::ResultSet(v)) => Ok(v),
        Ok(ProviderOutput::Acknowledge) => Ok(json!("ok")),
        Ok(other) => Ok(json!(format!("{other:?}"))),
        Err(e) => Err(e.to_string()),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let home = std::env::var("HOME").unwrap_or_default();
    let chat_db = std::path::Path::new(&home).join("Library/Messages/chat.db");
    let live = chat_db.exists()
        && rusqlite::Connection::open_with_flags(
            &chat_db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .and_then(|c| c.query_row("SELECT 1 FROM message LIMIT 1", [], |_| Ok(())))
        .is_ok();

    let dir = tempfile::TempDir::new()?;

    // ── The operator registers a database. This is the whole admin surface. ──
    let source_path = if live {
        chat_db.to_string_lossy().to_string()
    } else {
        let synth = dir.path().join("demo_source.db");
        let c = rusqlite::Connection::open(&synth)?;
        c.execute_batch(
            "CREATE TABLE message (ROWID INTEGER PRIMARY KEY, text TEXT, date INTEGER, is_from_me INT, handle_id INT);
             CREATE TABLE handle (ROWID INTEGER PRIMARY KEY, id TEXT);
             INSERT INTO handle VALUES (1, '+15551234567');
             INSERT INTO message VALUES (1,'hello there',748000000000000000,0,1),(2,'on my way',748000000000001000,1,1);",
        )?;
        synth.to_string_lossy().to_string()
    };

    println!(
        "\x1b[1mSource:\x1b[0m {} {}",
        source_path,
        if live {
            "(your real iMessage store)"
        } else {
            "(synthetic — chat.db not readable here)"
        }
    );

    let catalog = json!({"databases": [{
        "name": "imessage",
        "path": source_path,
        "mode": "ro",
        "description": "macOS iMessage store (read-only, snapshot-on-read)",
        "snapshot_on_read": true,
        "snapshot_ttl_secs": 300,
        "agents": { ARIEL: ["read"] }
    }]});
    std::fs::write(
        dir.path().join("db_catalog.json"),
        serde_json::to_string_pretty(&catalog)?,
    )?;
    println!("\x1b[1mCatalog:\x1b[0m registered 'imessage' as ro, granted {ARIEL} read");

    let p = SqliteTableProvider::new(dir.path())?;

    // ── 1. Discovery ────────────────────────────────────────────────────────
    step(1, "table.catalog — what can I reach?");
    println!("  (agent asks with no arguments; the answer is scoped to who is asking)");
    match run(&p, call("table.catalog", None, None, json!({}), ARIEL)).await {
        Ok(v) => show("as agent-ariel", &v),
        Err(e) => println!("  error: {e}"),
    }

    // ── 2. Schema discovery ─────────────────────────────────────────────────
    step(2, "table.schema — what is in it?");
    println!("  (omit the table name to list every table; this is how an agent learns a new DB)");
    match run(
        &p,
        call("table.schema", Some("imessage"), None, json!({}), ARIEL),
    )
    .await
    {
        Ok(v) => {
            let names: Vec<&str> = v["tables"]
                .as_array()
                .map(|a| a.iter().filter_map(|t| t["table"].as_str()).collect())
                .unwrap_or_default();
            println!("    {} tables: {}", names.len(), names.join(", "));
        }
        Err(e) => println!("  error: {e}"),
    }

    // ── 3. The builder — no SQL written by the agent ─────────────────────────
    step(3, "table.build — a query without writing SQL");
    let spec = json!({
        "table": {"table": "message", "as": "m"},
        "columns": ["m.ROWID", "m.date", "m.is_from_me"],
        "where": [{"column": "m.is_from_me", "op": "=", "value": 0}],
        "order_by": [{"column": "m.date", "dir": "desc"}],
        "limit": 3
    });
    show("spec sent", &spec);
    match run(&p, call("table.build", Some("imessage"), None, spec, ARIEL)).await {
        Ok(v) => {
            println!("    compiled SQL: {}", v["sql"].as_str().unwrap_or("?"));
            println!(
                "    rows returned: {}",
                v["rows"].as_array().map(|a| a.len()).unwrap_or(0)
            );
        }
        Err(e) => println!("  error: {e}"),
    }

    // ── 4. The power lane ───────────────────────────────────────────────────
    step(
        4,
        "table.query — raw SELECT when the builder cannot express it",
    );
    println!("  (Apple stores dates as ns since 2001; that arithmetic needs real SQL)");
    let sql = "SELECT datetime(m.date/1000000000 + 978307200, 'unixepoch', 'localtime') AS sent_at, \
               length(m.text) AS text_len, substr(h.id, -2) AS handle_tail \
               FROM message m JOIN handle h ON m.handle_id = h.ROWID \
               WHERE m.text IS NOT NULL ORDER BY m.date DESC";
    match run(
        &p,
        call(
            "table.query",
            Some("imessage"),
            Some(sql),
            json!({"limit": 3}),
            ARIEL,
        ),
    )
    .await
    {
        Ok(v) => show("rows (text redacted to length, handle masked)", &v),
        Err(e) => println!("  error: {e}"),
    }

    // ── 5. What the guardrails do ───────────────────────────────────────────
    step(5, "The refusals — what an agent cannot do");
    for (what, task) in [
        (
            "write task kind on a ro database",
            call(
                "table.exec",
                Some("imessage"),
                Some("DROP TABLE message"),
                json!({}),
                ARIEL,
            ),
        ),
        (
            "write smuggled through the read lane",
            call(
                "table.query",
                Some("imessage"),
                Some("DELETE FROM handle"),
                json!({}),
                ARIEL,
            ),
        ),
        (
            "ATTACH an arbitrary file via SQL",
            call(
                "table.query",
                Some("imessage"),
                Some("ATTACH DATABASE '/etc/passwd' AS x"),
                json!({}),
                ARIEL,
            ),
        ),
        (
            "SQL injected through the builder",
            call(
                "table.build",
                Some("imessage"),
                None,
                json!({"table": "message; DROP TABLE handle"}),
                ARIEL,
            ),
        ),
        (
            "a database the agent was not granted",
            call(
                "table.query",
                Some("imessage"),
                Some("SELECT 1"),
                json!({}),
                OTHER,
            ),
        ),
    ] {
        match run(&p, task).await {
            Ok(_) => println!("    \x1b[31mALLOWED (unexpected!)\x1b[0m  {what}"),
            Err(e) => println!(
                "    \x1b[32mrefused\x1b[0m  {what}\n              → {}",
                e.lines().next().unwrap_or("")
            ),
        }
    }

    step(6, "…and the ungranted agent cannot even see it exists");
    match run(&p, call("table.catalog", None, None, json!({}), OTHER)).await {
        Ok(v) => show(&format!("as {OTHER}"), &v),
        Err(e) => println!("  error: {e}"),
    }

    println!(
        "\n\x1b[1mSnapshot:\x1b[0m {} (0600, purged when the provider drops)",
        dir.path().join(".snapshots/imessage.db").display()
    );
    Ok(())
}
