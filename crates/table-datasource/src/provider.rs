use anyhow::{Result, bail};
use async_trait::async_trait;
use datasource::controller::{DatasourceProvider, DatasourceTask, ProviderOutput};
use rusqlite::{Connection, types::ValueRef};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::info;

pub struct SqliteTableProvider {
    db_path: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

impl SqliteTableProvider {
    pub fn open<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        let path = db_path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        info!(path = %path.display(), "table-datasource SQLite opened");
        Ok(Self {
            db_path: path,
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }
}

#[async_trait]
impl DatasourceProvider for SqliteTableProvider {
    fn id(&self) -> &str {
        "table"
    }

    fn supports(&self, task: &DatasourceTask) -> bool {
        matches!(
            task.kind.as_str(),
            "table.query"
                | "table.insert"
                | "table.configure"
                | "table.rolloff"
                | "table.stats"
                | "table.schema"
        )
    }

    async fn invoke(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        let conn = self.conn.lock().unwrap();
        match task.kind.as_str() {
            "table.configure" => configure_table(&conn, task),
            "table.query" => query_table(&conn, task),
            "table.insert" => insert_row(&conn, task),
            "table.rolloff" => rolloff_table(&conn, task),
            "table.stats" => table_stats(&conn, task),
            "table.schema" => table_schema(&conn, task),
            other => bail!("unsupported table task kind: {other}"),
        }
    }
}

// ── table.configure ──────────────────────────────────────────────────────────

fn configure_table(conn: &Connection, task: &DatasourceTask) -> Result<ProviderOutput> {
    let ddl = task.query.as_deref().ok_or_else(|| {
        anyhow::anyhow!("table.configure requires query field with CREATE TABLE SQL")
    })?;

    if !ddl.trim_start().to_uppercase().starts_with("CREATE TABLE") {
        bail!("table.configure query must be a CREATE TABLE statement");
    }

    conn.execute_batch(ddl)?;
    info!(ddl = &ddl[..ddl.len().min(120)], "table.configure executed");
    Ok(ProviderOutput::Acknowledge)
}

// ── table.query ──────────────────────────────────────────────────────────────

fn query_table(conn: &Connection, task: &DatasourceTask) -> Result<ProviderOutput> {
    let sql = task
        .query
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("table.query requires sql in query field"))?;

    let limit: usize = task
        .parameters
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(200) as usize;

    let mut stmt = conn.prepare(sql)?;
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();

    // Build owned boxed params from task.parameters (skip "limit" — that's our meta key).
    let owned_params: Vec<Box<dyn rusqlite::types::ToSql>> = task
        .parameters
        .as_object()
        .map(|obj| {
            obj.iter()
                .filter(|(k, _)| *k != "limit")
                .map(|(_, v)| json_to_sql_box(v))
                .collect()
        })
        .unwrap_or_default();

    let rows_result = stmt.query_map(
        rusqlite::params_from_iter(owned_params.iter().map(|p| p.as_ref())),
        |row| {
            let mut obj = Map::new();
            for (i, col) in col_names.iter().enumerate() {
                let val = match row.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(n) => Value::Number(n.into()),
                    ValueRef::Real(f) => {
                        Value::Number(serde_json::Number::from_f64(f).unwrap_or(0.into()))
                    }
                    ValueRef::Text(s) => Value::String(String::from_utf8_lossy(s).into_owned()),
                    ValueRef::Blob(b) => Value::String(format!("<blob {} bytes>", b.len())),
                };
                obj.insert(col.clone(), val);
            }
            Ok(obj)
        },
    );

    match rows_result {
        Ok(rows) => {
            let mut records: Vec<Value> = Vec::new();
            for row in rows {
                if records.len() >= limit {
                    break;
                }
                records.push(Value::Object(row?));
            }
            Ok(ProviderOutput::ResultSet(Value::Array(records)))
        }
        Err(e) => bail!("table.query failed: {e}"),
    }
}

// ── table.insert ─────────────────────────────────────────────────────────────

fn insert_row(conn: &Connection, task: &DatasourceTask) -> Result<ProviderOutput> {
    let table = task
        .graph_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("table.insert requires graph_id (table name)"))?;
    validate_identifier(table)?;

    let row = task
        .parameters
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("table.insert requires parameters as JSON object (row)"))?;

    if row.is_empty() {
        bail!("table.insert row cannot be empty");
    }

    let cols: Vec<&str> = row.keys().map(String::as_str).collect();
    let placeholders: Vec<String> = (1..=cols.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "INSERT INTO {table} ({cols}) VALUES ({vals})",
        cols = cols.join(", "),
        vals = placeholders.join(", ")
    );

    let values: Vec<Box<dyn rusqlite::types::ToSql>> = cols
        .iter()
        .map(|col| json_to_sql_box(row.get(*col).unwrap()))
        .collect();

    conn.execute(
        &sql,
        rusqlite::params_from_iter(values.iter().map(|v| v.as_ref())),
    )?;

    Ok(ProviderOutput::Acknowledge)
}

// ── table.rolloff ─────────────────────────────────────────────────────────────

fn rolloff_table(conn: &Connection, task: &DatasourceTask) -> Result<ProviderOutput> {
    let table = task
        .graph_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("table.rolloff requires graph_id (table name)"))?;
    validate_identifier(table)?;

    let params = &task.parameters;
    let ts_col = params
        .get("ts_column")
        .and_then(Value::as_str)
        .unwrap_or("timestamp");
    validate_identifier(ts_col)?;

    let mut deleted: usize = 0;

    if let Some(max_rows) = params.get("max_rows").and_then(Value::as_u64) {
        // Keep only the newest max_rows rows.
        let sql = format!(
            "DELETE FROM {table} WHERE rowid NOT IN \
             (SELECT rowid FROM {table} ORDER BY {ts_col} DESC LIMIT ?1)"
        );
        deleted += conn.execute(&sql, rusqlite::params![max_rows as i64])?;
    }

    if let Some(max_age_secs) = params.get("max_age_secs").and_then(Value::as_u64) {
        let cutoff = now_epoch_secs().saturating_sub(max_age_secs);
        let sql = format!("DELETE FROM {table} WHERE {ts_col} < ?1");
        deleted += conn.execute(&sql, rusqlite::params![cutoff as i64])?;
    }

    if deleted > 0 {
        info!(table, deleted, "table.rolloff complete");
    }

    Ok(ProviderOutput::ResultSet(json!({"deleted": deleted})))
}

// ── table.stats ──────────────────────────────────────────────────────────────

fn table_stats(conn: &Connection, task: &DatasourceTask) -> Result<ProviderOutput> {
    let table = task
        .graph_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("table.stats requires graph_id (table name)"))?;
    validate_identifier(table)?;

    let ts_col = task
        .parameters
        .get("ts_column")
        .and_then(Value::as_str)
        .unwrap_or("timestamp");
    validate_identifier(ts_col)?;

    let count: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })?;

    let latest: Option<i64> = conn
        .query_row(&format!("SELECT MAX({ts_col}) FROM {table}"), [], |row| {
            row.get(0)
        })
        .ok();

    Ok(ProviderOutput::ResultSet(json!({
        "table": table,
        "row_count": count,
        "latest_ts": latest,
    })))
}

// ── table.schema ─────────────────────────────────────────────────────────────

fn table_schema(conn: &Connection, task: &DatasourceTask) -> Result<ProviderOutput> {
    let table = task
        .graph_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("table.schema requires graph_id (table name)"))?;
    validate_identifier(table)?;

    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
            rusqlite::params![table],
            |row| row.get(0),
        )
        .map_err(|_| anyhow::anyhow!("table '{table}' not found"))?;

    Ok(ProviderOutput::ResultSet(
        json!({ "table": table, "schema_sql": sql }),
    ))
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn validate_identifier(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains('"')
        || name.contains(';')
        || name.contains("--")
        || name.contains('\0')
    {
        bail!("invalid SQL identifier: {name:?}");
    }
    Ok(())
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn json_to_sql_box(v: &Value) -> Box<dyn rusqlite::types::ToSql> {
    match v {
        Value::Null => Box::new(rusqlite::types::Null),
        Value::Bool(b) => Box::new(*b as i64),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Box::new(i)
            } else if let Some(f) = n.as_f64() {
                Box::new(f)
            } else {
                Box::new(n.to_string())
            }
        }
        Value::String(s) => Box::new(s.clone()),
        other => Box::new(other.to_string()),
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use datasource::controller::DatasourceTask;
    use serde_json::json;
    use tempfile::NamedTempFile;

    fn open_tmp() -> (SqliteTableProvider, NamedTempFile) {
        let f = NamedTempFile::new().unwrap();
        let p = SqliteTableProvider::open(f.path()).unwrap();
        (p, f)
    }

    fn make_task(
        kind: &str,
        graph_id: Option<&str>,
        query: Option<&str>,
        params: Value,
    ) -> DatasourceTask {
        DatasourceTask {
            kind: datasource::controller::TaskKind::Custom(kind.to_string()),
            provider: Some("table".to_string()),
            graph_id: graph_id.map(str::to_string),
            query: query.map(str::to_string),
            parameters: params,
            identity: json!({}),
        }
    }

    #[tokio::test]
    async fn configure_and_insert_and_query() {
        let (p, _f) = open_tmp();

        p.invoke(&make_task(
            "table.configure",
            None,
            Some("CREATE TABLE IF NOT EXISTS signals (id INTEGER PRIMARY KEY AUTOINCREMENT, provider TEXT NOT NULL, latency_ms INTEGER, timestamp INTEGER NOT NULL)"),
            json!({}),
        )).await.unwrap();

        p.invoke(&make_task(
            "table.insert",
            Some("signals"),
            None,
            json!({"provider": "gemini", "latency_ms": 120, "timestamp": 1_000_000}),
        ))
        .await
        .unwrap();

        p.invoke(&make_task(
            "table.insert",
            Some("signals"),
            None,
            json!({"provider": "ollama", "latency_ms": 45, "timestamp": 1_000_001}),
        ))
        .await
        .unwrap();

        let out = p
            .invoke(&make_task(
                "table.query",
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
    async fn rolloff_by_max_rows() {
        let (p, _f) = open_tmp();

        p.invoke(&make_task(
            "table.configure",
            None,
            Some("CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts INTEGER NOT NULL, val TEXT)"),
            json!({}),
        )).await.unwrap();

        for i in 0..10u64 {
            p.invoke(&make_task(
                "table.insert",
                Some("events"),
                None,
                json!({"ts": i, "val": format!("row-{i}")}),
            ))
            .await
            .unwrap();
        }

        p.invoke(&make_task(
            "table.rolloff",
            Some("events"),
            None,
            json!({"max_rows": 5, "ts_column": "ts"}),
        ))
        .await
        .unwrap();

        let out = p
            .invoke(&make_task(
                "table.stats",
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
    async fn stats_returns_count_and_latest_ts() {
        let (p, _f) = open_tmp();
        p.invoke(&make_task(
            "table.configure",
            None,
            Some("CREATE TABLE IF NOT EXISTS t (ts INTEGER NOT NULL)"),
            json!({}),
        ))
        .await
        .unwrap();
        p.invoke(&make_task(
            "table.insert",
            Some("t"),
            None,
            json!({"ts": 999}),
        ))
        .await
        .unwrap();

        let out = p
            .invoke(&make_task(
                "table.stats",
                Some("t"),
                None,
                json!({"ts_column":"ts"}),
            ))
            .await
            .unwrap();
        let ProviderOutput::ResultSet(s) = out else {
            panic!()
        };
        assert_eq!(s["row_count"], 1);
        assert_eq!(s["latest_ts"], 999);
    }

    #[tokio::test]
    async fn schema_returns_ddl() {
        let (p, _f) = open_tmp();
        p.invoke(&make_task(
            "table.configure",
            None,
            Some("CREATE TABLE IF NOT EXISTS meta (id TEXT PRIMARY KEY, val TEXT)"),
            json!({}),
        ))
        .await
        .unwrap();

        let out = p
            .invoke(&make_task("table.schema", Some("meta"), None, json!({})))
            .await
            .unwrap();
        let ProviderOutput::ResultSet(s) = out else {
            panic!()
        };
        assert!(s["schema_sql"].as_str().unwrap().contains("CREATE TABLE"));
    }

    #[tokio::test]
    async fn rejects_invalid_identifier() {
        let (p, _f) = open_tmp();
        let result = p
            .invoke(&make_task(
                "table.rolloff",
                Some("bad\"table"),
                None,
                json!({"max_rows": 1}),
            ))
            .await;
        assert!(result.is_err());
    }
}
