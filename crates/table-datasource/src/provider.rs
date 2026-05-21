use anyhow::{Result, bail};
use async_trait::async_trait;
use datasource::controller::{DatasourceProvider, DatasourceTask, ProviderOutput};
use rusqlite::{Connection, types::ValueRef};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::info;

pub struct SqliteTableProvider {
    base_dir: PathBuf,
    pool: Arc<Mutex<HashMap<String, Arc<Mutex<Connection>>>>>,
}

impl SqliteTableProvider {
    /// Directory-based provider — DB name maps to `{base_dir}/{name}.db`.
    pub fn new(base_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(base_dir)?;
        info!(base_dir = %base_dir.display(), "table-datasource initialised (multi-DB)");
        Ok(Self {
            base_dir: base_dir.to_path_buf(),
            pool: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Single-file mode — opens `db_path` as the "default" connection.
    /// Used for backward compat and tests.
    pub fn open<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        let path = db_path.as_ref();
        let base_dir = path.parent().unwrap_or(Path::new("."));
        let provider = Self {
            base_dir: base_dir.to_path_buf(),
            pool: Arc::new(Mutex::new(HashMap::new())),
        };
        // Pre-open the specific file as "default".
        provider.open_named("default", path)?;
        Ok(provider)
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    fn open_named(&self, name: &str, path: &Path) -> Result<Arc<Mutex<Connection>>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        info!(db = name, path = %path.display(), "table-datasource opened DB");
        let conn = Arc::new(Mutex::new(conn));
        self.pool.lock().unwrap().insert(name.to_string(), conn.clone());
        Ok(conn)
    }

    fn get_conn(&self, task: &DatasourceTask) -> Result<Arc<Mutex<Connection>>> {
        let name = task.db.as_deref().unwrap_or("default");
        if let Some(conn) = self.pool.lock().unwrap().get(name) {
            return Ok(conn.clone());
        }
        let path = self.base_dir.join(format!("{}.db", name));
        self.open_named(name, &path)
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
                | "table.upsert"
                | "table.update"
                | "table.delete"
                | "table.exec"
                | "table.configure"
                | "table.rolloff"
                | "table.stats"
                | "table.schema"
                | "table.list"
        )
    }

    async fn invoke(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        match task.kind.as_str() {
            "table.list" => return list_dbs(self, task),
            _ => {}
        }
        let conn_arc = self.get_conn(task)?;
        let conn = conn_arc.lock().unwrap();
        match task.kind.as_str() {
            "table.configure" | "table.exec" => exec_ddl(&conn, task),
            "table.query" => query_table(&conn, task),
            "table.insert" => insert_row(&conn, task, false),
            "table.upsert" => insert_row(&conn, task, true),
            "table.update" => update_rows(&conn, task),
            "table.delete" => delete_rows(&conn, task),
            "table.rolloff" => rolloff_table(&conn, task),
            "table.stats" => table_stats(&conn, task),
            "table.schema" => table_schema(&conn, task),
            other => bail!("unsupported table task kind: {other}"),
        }
    }
}

// ── table.configure / table.exec ─────────────────────────────────────────────

fn exec_ddl(conn: &Connection, task: &DatasourceTask) -> Result<ProviderOutput> {
    let ddl = task.query.as_deref().ok_or_else(|| {
        anyhow::anyhow!("table.exec / table.configure requires SQL in query field")
    })?;
    conn.execute_batch(ddl)?;
    info!(ddl = &ddl[..ddl.len().min(120)], "table DDL executed");
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

// ── table.insert / table.upsert ───────────────────────────────────────────────

fn insert_row(conn: &Connection, task: &DatasourceTask, or_replace: bool) -> Result<ProviderOutput> {
    let table = task
        .graph_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("table.insert/upsert requires graph_id (table name)"))?;
    validate_identifier(table)?;

    let row = task
        .parameters
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("table.insert/upsert requires parameters as JSON object (row)"))?;

    if row.is_empty() {
        bail!("table.insert/upsert row cannot be empty");
    }

    let cols: Vec<&str> = row.keys().map(String::as_str).collect();
    let placeholders: Vec<String> = (1..=cols.len()).map(|i| format!("?{i}")).collect();
    let keyword = if or_replace { "INSERT OR REPLACE" } else { "INSERT" };
    let sql = format!(
        "{keyword} INTO {table} ({cols}) VALUES ({vals})",
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

// ── table.update ─────────────────────────────────────────────────────────────

/// parameters shape:
///   { "set": { col: val, ... }, "where": "col = ?1", "params": [val, ...] }
fn update_rows(conn: &Connection, task: &DatasourceTask) -> Result<ProviderOutput> {
    let table = task
        .graph_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("table.update requires graph_id (table name)"))?;
    validate_identifier(table)?;

    let set_obj = task
        .parameters
        .get("set")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("table.update requires parameters.set as JSON object"))?;

    if set_obj.is_empty() {
        bail!("table.update set cannot be empty");
    }

    let mut all_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut set_clauses: Vec<String> = Vec::new();
    for (col, val) in set_obj {
        validate_identifier(col)?;
        let idx = all_values.len() + 1;
        set_clauses.push(format!("{col} = ?{idx}"));
        all_values.push(json_to_sql_box(val));
    }

    let where_clause = task.parameters.get("where").and_then(Value::as_str);
    let where_params = task
        .parameters
        .get("params")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // Re-index WHERE ?N params to follow SET params.
    let offset = all_values.len();
    let where_part = if let Some(wc) = where_clause {
        for p in &where_params {
            all_values.push(json_to_sql_box(p));
        }
        // Rewrite ?1, ?2... in where clause to ?{offset+1}, ?{offset+2}...
        reindex_params(wc, offset)
    } else {
        String::new()
    };

    let sql = if where_part.is_empty() {
        format!("UPDATE {table} SET {}", set_clauses.join(", "))
    } else {
        format!("UPDATE {table} SET {} WHERE {where_part}", set_clauses.join(", "))
    };

    let affected = conn.execute(
        &sql,
        rusqlite::params_from_iter(all_values.iter().map(|v| v.as_ref())),
    )?;

    Ok(ProviderOutput::ResultSet(json!({ "rows_affected": affected })))
}

// ── table.delete ─────────────────────────────────────────────────────────────

/// query = WHERE clause SQL (optional — omit to delete all rows)
/// parameters = JSON array of positional params for WHERE
fn delete_rows(conn: &Connection, task: &DatasourceTask) -> Result<ProviderOutput> {
    let table = task
        .graph_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("table.delete requires graph_id (table name)"))?;
    validate_identifier(table)?;

    let sql = match task.query.as_deref() {
        Some(wc) if !wc.trim().is_empty() => format!("DELETE FROM {table} WHERE {wc}"),
        _ => format!("DELETE FROM {table}"),
    };

    let owned_params: Vec<Box<dyn rusqlite::types::ToSql>> = match &task.parameters {
        Value::Array(arr) => arr.iter().map(json_to_sql_box).collect(),
        _ => Vec::new(),
    };

    let affected = conn.execute(
        &sql,
        rusqlite::params_from_iter(owned_params.iter().map(|v| v.as_ref())),
    )?;

    Ok(ProviderOutput::ResultSet(json!({ "rows_affected": affected })))
}

// ── table.list ───────────────────────────────────────────────────────────────

fn list_dbs(provider: &SqliteTableProvider, _task: &DatasourceTask) -> Result<ProviderOutput> {
    let pool = provider.pool.lock().unwrap();
    let dbs: Vec<Value> = pool
        .keys()
        .map(|name| json!({ "db": name }))
        .collect();
    Ok(ProviderOutput::ResultSet(json!({ "databases": dbs })))
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

/// Re-index ?1, ?2 in a WHERE clause to start at offset+1.
fn reindex_params(clause: &str, offset: usize) -> String {
    if offset == 0 {
        return clause.to_string();
    }
    // Replace ?N with ?(N+offset). Simple regex-free replacement.
    let mut out = String::with_capacity(clause.len() + 8);
    let mut chars = clause.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '?' {
            let mut digits = String::new();
            while chars.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                digits.push(chars.next().unwrap());
            }
            if digits.is_empty() {
                out.push('?');
            } else {
                let n: usize = digits.parse().unwrap_or(1);
                out.push('?');
                out.push_str(&(n + offset).to_string());
            }
        } else {
            out.push(ch);
        }
    }
    out
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
        DatasourceTask {
            kind: datasource::controller::TaskKind::Custom(kind.to_string()),
            provider: Some("table".to_string()),
            db: db.map(str::to_string),
            graph_id: graph_id.map(str::to_string),
            query: query.map(str::to_string),
            parameters: params,
            identity: json!({}),
        }
    }

    #[tokio::test]
    async fn configure_and_insert_and_query() {
        let (p, _d) = open_tmp();

        p.invoke(&make_task(
            "table.configure",
            None,
            None,
            Some("CREATE TABLE IF NOT EXISTS signals (id INTEGER PRIMARY KEY AUTOINCREMENT, provider TEXT NOT NULL, latency_ms INTEGER, timestamp INTEGER NOT NULL)"),
            json!({}),
        )).await.unwrap();

        p.invoke(&make_task(
            "table.insert",
            None,
            Some("signals"),
            None,
            json!({"provider": "gemini", "latency_ms": 120, "timestamp": 1_000_000}),
        ))
        .await
        .unwrap();

        p.invoke(&make_task(
            "table.insert",
            None,
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

        p.invoke(&make_task(
            "table.upsert",
            None,
            Some("kv"),
            None,
            json!({"id": "k1", "val": "first"}),
        ))
        .await
        .unwrap();

        p.invoke(&make_task(
            "table.upsert",
            None,
            Some("kv"),
            None,
            json!({"id": "k1", "val": "second"}),
        ))
        .await
        .unwrap();

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

        let ProviderOutput::ResultSet(r) = out else { panic!() };
        assert_eq!(r["rows_affected"], 1);

        let query_out = p
            .invoke(&make_task(
                "table.query",
                None,
                None,
                Some("SELECT id FROM items WHERE status = 1"),
                json!({}),
            ))
            .await
            .unwrap();
        let ProviderOutput::ResultSet(Value::Array(rows)) = query_out else { panic!() };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], "b");
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
            p.invoke(&make_task("table.insert", None, Some("items"), None, json!({"id": id})))
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
        let ProviderOutput::ResultSet(r) = out else { panic!() };
        assert_eq!(r["rows_affected"], 1);

        let query_out = p
            .invoke(&make_task(
                "table.query",
                None,
                None,
                Some("SELECT COUNT(*) as n FROM items"),
                json!({}),
            ))
            .await
            .unwrap();
        let ProviderOutput::ResultSet(Value::Array(rows)) = query_out else { panic!() };
        assert_eq!(rows[0]["n"], 2);
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
        let ProviderOutput::ResultSet(Value::Array(rows)) = out else { panic!() };
        assert_eq!(rows[0]["n"], 0, "db_beta must be empty");
    }

    #[tokio::test]
    async fn exec_creates_index() {
        let (p, _d) = open_tmp();

        p.invoke(&make_task(
            "table.exec",
            None,
            None,
            Some("CREATE TABLE IF NOT EXISTS events (id TEXT PRIMARY KEY, ts INTEGER)"),
            json!({}),
        ))
        .await
        .unwrap();

        p.invoke(&make_task(
            "table.exec",
            None,
            None,
            Some("CREATE INDEX IF NOT EXISTS idx_ts ON events (ts)"),
            json!({}),
        ))
        .await
        .unwrap();
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
    async fn stats_returns_count_and_latest_ts() {
        let (p, _d) = open_tmp();
        p.invoke(&make_task(
            "table.configure",
            None,
            None,
            Some("CREATE TABLE IF NOT EXISTS t (ts INTEGER NOT NULL)"),
            json!({}),
        ))
        .await
        .unwrap();
        p.invoke(&make_task(
            "table.insert",
            None,
            Some("t"),
            None,
            json!({"ts": 999}),
        ))
        .await
        .unwrap();

        let out = p
            .invoke(&make_task(
                "table.stats",
                None,
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
            .invoke(&make_task("table.schema", None, Some("meta"), None, json!({})))
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
}
