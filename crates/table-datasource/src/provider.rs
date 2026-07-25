use anyhow::{Result, bail};
use async_trait::async_trait;
use datasource::controller::{DatasourceProvider, DatasourceTask, ProviderOutput};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::{Connection, OpenFlags, types::ValueRef};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::info;

use crate::builder::build_query;
use crate::catalog::{
    Catalog, CatalogEntry, DbMode, Verb, agent_from_identity, validate_profile_db_name,
    verb_for_kind,
};

/// One pooled database connection plus its enforcement state.
#[derive(Clone)]
struct DbHandle {
    conn: Arc<Mutex<Connection>>,
    mode: DbMode,
    /// Gate for ATTACH/DETACH: the runner flips this while performing its own
    /// catalog-resolved attaches; agent SQL hitting ATTACH is denied by the
    /// connection authorizer whenever it is false.
    attach_ok: Arc<AtomicBool>,
}

pub struct SqliteTableProvider {
    base_dir: PathBuf,
    catalog: Mutex<Catalog>,
    pool: Arc<Mutex<HashMap<String, DbHandle>>>,
}

impl SqliteTableProvider {
    /// Directory-based provider — profile DB name maps to `{base_dir}/{name}.db`;
    /// catalog databases come from `{base_dir}/db_catalog.json` (or
    /// `PHILOTIC_TABLE_CATALOG`).
    pub fn new(base_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(base_dir)?;
        let catalog = Catalog::load(Catalog::default_path(base_dir));
        info!(base_dir = %base_dir.display(), "table-datasource initialised (multi-DB + catalog)");
        Ok(Self {
            base_dir: base_dir.to_path_buf(),
            catalog: Mutex::new(catalog),
            pool: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Single-file mode — opens `db_path` as the "default" connection.
    /// Used for backward compat and tests.
    pub fn open<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        let path = db_path.as_ref();
        let base_dir = path.parent().unwrap_or(Path::new("."));
        let provider = Self::new(base_dir)?;
        let handle = open_handle(path, DbMode::ReadWrite)?;
        provider
            .pool
            .lock()
            .unwrap()
            .insert("default".to_string(), handle);
        Ok(provider)
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Reload the catalog when its file changed; drop pooled connections so
    /// path/mode/grant changes take effect without a restart.
    fn refresh_catalog(&self) {
        if self.catalog.lock().unwrap().refresh_if_changed() {
            self.pool.lock().unwrap().clear();
            info!("db_catalog.json changed — catalog reloaded, connection pool reset");
        }
    }

    fn catalog_entry(&self, name: &str) -> Option<CatalogEntry> {
        self.catalog.lock().unwrap().get(name).cloned()
    }

    fn get_handle(&self, name: &str, agent: &str, verb: Verb) -> Result<DbHandle> {
        if let Some(entry) = self.catalog_entry(name) {
            let mode = entry.db_mode()?;
            if mode == DbMode::ReadOnly && verb == Verb::Write {
                bail!("database '{name}' is read-only");
            }
            if !entry.allows(agent, verb) {
                bail!("agent '{agent}' has no {verb:?} grant on database '{name}'");
            }
            let src = entry.resolved_path(&self.base_dir);
            if !src.exists() {
                bail!("catalog database '{name}' missing at {}", src.display());
            }
            let (open_path, refreshed) = if entry.snapshot_on_read {
                self.ensure_snapshot(&entry, &src)?
            } else {
                (src, false)
            };
            if refreshed {
                self.pool.lock().unwrap().remove(name);
            }
            if let Some(handle) = self.pool.lock().unwrap().get(name) {
                return Ok(handle.clone());
            }
            let handle = open_handle(&open_path, mode)?;
            self.pool
                .lock()
                .unwrap()
                .insert(name.to_string(), handle.clone());
            return Ok(handle);
        }

        // Profile database: legacy implicit mapping, now name-sanitized.
        validate_profile_db_name(name)?;
        if let Some(handle) = self.pool.lock().unwrap().get(name) {
            return Ok(handle.clone());
        }
        let path = self.base_dir.join(format!("{name}.db"));
        // Write kinds still create on demand (that is how tables get made), but
        // a *read* must never bring a database into existence — otherwise any
        // agent probing names litters the profile directory with empty files
        // that then show up in table.catalog.
        if verb == Verb::Read && !path.exists() {
            bail!("unknown database {name:?}");
        }
        let handle = open_handle(&path, DbMode::ReadWrite)?;
        self.pool
            .lock()
            .unwrap()
            .insert(name.to_string(), handle.clone());
        Ok(handle)
    }

    /// Copy-on-read for live-written databases: back the source up into
    /// `.snapshots/{name}.db` and query the copy. Returns the path to open and
    /// whether the snapshot was (re)taken this call.
    fn ensure_snapshot(&self, entry: &CatalogEntry, src: &Path) -> Result<(PathBuf, bool)> {
        let snap_dir = self.base_dir.join(".snapshots");
        std::fs::create_dir_all(&snap_dir)?;
        let snap = snap_dir.join(format!("{}.db", entry.name));

        let fresh = std::fs::metadata(&snap)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|mtime| mtime.elapsed().ok())
            .map(|age| age < Duration::from_secs(entry.snapshot_ttl_secs))
            .unwrap_or(false);
        if fresh {
            return Ok((snap, false));
        }

        let src_conn = Connection::open_with_flags(
            src,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )?;
        let _ = std::fs::remove_file(&snap);
        let mut dst_conn = Connection::open(&snap)?;
        let backup = rusqlite::backup::Backup::new(&src_conn, &mut dst_conn)?;
        backup.run_to_completion(256, Duration::from_millis(5), None)?;
        info!(db = entry.name, src = %src.display(), "snapshot refreshed");
        Ok((snap, true))
    }

    /// Resolve `attach` database names to (alias, path) pairs, enforcing a read
    /// grant on every attached database. Attached databases are always opened
    /// read-only regardless of their own mode.
    fn resolve_attaches(
        &self,
        names: &[String],
        agent: &str,
        primary: &str,
    ) -> Result<Vec<(String, PathBuf)>> {
        let mut out = Vec::new();
        for name in names {
            if name == primary {
                continue;
            }
            if let Some(entry) = self.catalog_entry(name) {
                if !entry.allows(agent, Verb::Read) {
                    bail!("agent '{agent}' has no Read grant on database '{name}'");
                }
                let src = entry.resolved_path(&self.base_dir);
                if !src.exists() {
                    bail!("catalog database '{name}' missing at {}", src.display());
                }
                let (path, _) = if entry.snapshot_on_read {
                    self.ensure_snapshot(&entry, &src)?
                } else {
                    (src, false)
                };
                out.push((name.clone(), path));
            } else {
                validate_profile_db_name(name)?;
                let path = self.base_dir.join(format!("{name}.db"));
                if !path.exists() {
                    bail!("cannot attach unknown database '{name}'");
                }
                out.push((name.clone(), path));
            }
        }
        Ok(out)
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
                | "table.build"
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
                | "table.catalog"
        )
    }

    async fn invoke(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        let kind = task.kind.as_str();
        let verb = verb_for_kind(kind)?;
        let agent = agent_from_identity(&task.identity);
        self.refresh_catalog();

        match kind {
            "table.list" => return list_dbs(self, &agent),
            "table.catalog" => return catalog_info(self, &agent),
            _ => {}
        }

        let db_name = task.db.as_deref().unwrap_or("default");
        let handle = self.get_handle(db_name, &agent, verb)?;

        if kind == "table.build" {
            let built = build_query(&task.parameters)?;
            let attaches = self.resolve_attaches(&built.attach_dbs, &agent, db_name)?;
            let conn = handle.conn.lock().unwrap();
            return with_attached(&conn, &handle, &attaches, |conn| {
                run_read_query(conn, &built.sql, &built.params, usize::MAX).map(|rows| {
                    ProviderOutput::ResultSet(json!({ "sql": built.sql, "rows": rows }))
                })
            });
        }

        // table.query may also attach catalog databases: parameters.attach = ["name", ...]
        let attach_names: Vec<String> = if kind == "table.query" {
            task.parameters
                .get("attach")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let attaches = self.resolve_attaches(&attach_names, &agent, db_name)?;

        let conn = handle.conn.lock().unwrap();
        with_attached(&conn, &handle, &attaches, |conn| match kind {
            "table.configure" | "table.exec" => exec_ddl(conn, task),
            "table.query" => query_table(conn, task),
            "table.insert" => insert_row(conn, task, false),
            "table.upsert" => insert_row(conn, task, true),
            "table.update" => update_rows(conn, task),
            "table.delete" => delete_rows(conn, task),
            "table.rolloff" => rolloff_table(conn, task),
            "table.stats" => table_stats(conn, task),
            "table.schema" => table_schema(conn, task),
            other => bail!("unsupported table task kind: {other}"),
        })
    }
}

// ── connection plumbing ──────────────────────────────────────────────────────

fn open_handle(path: &Path, mode: DbMode) -> Result<DbHandle> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = match mode {
        DbMode::ReadWrite => {
            let conn = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_CREATE
                    | OpenFlags::SQLITE_OPEN_URI,
            )?;
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
            conn
        }
        DbMode::ReadOnly => {
            let conn = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            )?;
            conn.execute_batch("PRAGMA query_only=ON;")?;
            conn
        }
    };

    let attach_ok = Arc::new(AtomicBool::new(false));
    let flag = attach_ok.clone();
    conn.authorizer(Some(move |ctx: AuthContext<'_>| match ctx.action {
        AuthAction::Attach { .. } | AuthAction::Detach { .. } => {
            if flag.load(Ordering::Relaxed) {
                Authorization::Allow
            } else {
                Authorization::Deny
            }
        }
        _ => Authorization::Allow,
    }));

    info!(path = %path.display(), ro = matches!(mode, DbMode::ReadOnly), "table-datasource opened DB");
    Ok(DbHandle {
        conn: Arc::new(Mutex::new(conn)),
        mode,
        attach_ok,
    })
}

/// Run `f` with the given catalog databases attached read-only, detaching them
/// afterwards even when `f` fails. ATTACH/DETACH are only authorized while the
/// handle's gate flag is up — agent SQL can never attach on its own.
fn with_attached<F>(
    conn: &Connection,
    handle: &DbHandle,
    attaches: &[(String, PathBuf)],
    f: F,
) -> Result<ProviderOutput>
where
    F: FnOnce(&Connection) -> Result<ProviderOutput>,
{
    for (alias, path) in attaches {
        handle.attach_ok.store(true, Ordering::Relaxed);
        let uri = format!("file:{}?mode=ro", path.display());
        let attached = conn.execute(
            &format!("ATTACH DATABASE ?1 AS {alias}"),
            rusqlite::params![uri],
        );
        handle.attach_ok.store(false, Ordering::Relaxed);
        if let Err(err) = attached {
            detach_all(conn, handle, attaches);
            return Err(anyhow::anyhow!(
                "failed to attach database '{alias}': {err}"
            ));
        }
    }

    let result = f(conn);
    detach_all(conn, handle, attaches);
    result
}

fn detach_all(conn: &Connection, handle: &DbHandle, attaches: &[(String, PathBuf)]) {
    for (alias, _) in attaches {
        handle.attach_ok.store(true, Ordering::Relaxed);
        let _ = conn.execute(&format!("DETACH DATABASE {alias}"), []);
        handle.attach_ok.store(false, Ordering::Relaxed);
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

    let params: Vec<Value> = task
        .parameters
        .as_object()
        .map(|obj| {
            obj.iter()
                .filter(|(k, _)| *k != "limit" && *k != "attach")
                .map(|(_, v)| v.clone())
                .collect()
        })
        .unwrap_or_default();

    let rows = run_read_query(conn, sql, &params, limit)?;
    Ok(ProviderOutput::ResultSet(Value::Array(rows)))
}

/// The shared read lane: prepares `sql`, refuses anything that is not a
/// read-only statement, binds `params` positionally, and maps rows to JSON.
fn run_read_query(
    conn: &Connection,
    sql: &str,
    params: &[Value],
    limit: usize,
) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(sql)?;
    if !stmt.readonly() {
        bail!("table.query/table.build are read-only — use the explicit write task kinds");
    }
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();

    let owned_params: Vec<Box<dyn rusqlite::types::ToSql>> =
        params.iter().map(json_to_sql_box).collect();

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
            Ok(records)
        }
        Err(e) => bail!("query failed: {e}"),
    }
}

// ── table.insert / table.upsert ───────────────────────────────────────────────

fn insert_row(
    conn: &Connection,
    task: &DatasourceTask,
    or_replace: bool,
) -> Result<ProviderOutput> {
    let table = task
        .graph_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("table.insert/upsert requires graph_id (table name)"))?;
    validate_identifier(table)?;

    let row = task.parameters.as_object().ok_or_else(|| {
        anyhow::anyhow!("table.insert/upsert requires parameters as JSON object (row)")
    })?;

    if row.is_empty() {
        bail!("table.insert/upsert row cannot be empty");
    }

    let cols: Vec<&str> = row.keys().map(String::as_str).collect();
    let placeholders: Vec<String> = (1..=cols.len()).map(|i| format!("?{i}")).collect();
    let keyword = if or_replace {
        "INSERT OR REPLACE"
    } else {
        "INSERT"
    };
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
        format!(
            "UPDATE {table} SET {} WHERE {where_part}",
            set_clauses.join(", ")
        )
    };

    let affected = conn.execute(
        &sql,
        rusqlite::params_from_iter(all_values.iter().map(|v| v.as_ref())),
    )?;

    Ok(ProviderOutput::ResultSet(
        json!({ "rows_affected": affected }),
    ))
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

    Ok(ProviderOutput::ResultSet(
        json!({ "rows_affected": affected }),
    ))
}

// ── table.list / table.catalog ───────────────────────────────────────────────

fn list_dbs(provider: &SqliteTableProvider, agent: &str) -> Result<ProviderOutput> {
    let mut seen: Vec<String> = Vec::new();
    let mut dbs: Vec<Value> = Vec::new();

    {
        let catalog = provider.catalog.lock().unwrap();
        for entry in catalog.entries() {
            if entry.allows(agent, Verb::Read) {
                seen.push(entry.name.clone());
                dbs.push(json!({ "db": entry.name, "source": "catalog" }));
            }
        }
    }
    let pool = provider.pool.lock().unwrap();
    for name in pool.keys() {
        if !seen.contains(name) {
            dbs.push(json!({ "db": name, "source": "profile" }));
        }
    }
    Ok(ProviderOutput::ResultSet(json!({ "databases": dbs })))
}

/// Rich catalog view for the calling agent: what exists, what mode it is in,
/// and which verbs the caller holds on it.
fn catalog_info(provider: &SqliteTableProvider, agent: &str) -> Result<ProviderOutput> {
    let mut dbs: Vec<Value> = Vec::new();

    {
        let catalog = provider.catalog.lock().unwrap();
        for entry in catalog.entries() {
            let read = entry.allows(agent, Verb::Read);
            let write = entry.allows(agent, Verb::Write);
            if !read && !write {
                continue;
            }
            let mut verbs: Vec<&str> = Vec::new();
            if read {
                verbs.push("read");
            }
            if write {
                verbs.push("write");
            }
            dbs.push(json!({
                "db": entry.name,
                "source": "catalog",
                "mode": entry.mode,
                "description": entry.description,
                "snapshot_on_read": entry.snapshot_on_read,
                "verbs": verbs,
            }));
        }
    }

    // Profile databases: every *.db file in the profile dir (rw, implicit).
    if let Ok(read_dir) = std::fs::read_dir(&provider.base_dir) {
        for dir_entry in read_dir.flatten() {
            let path = dir_entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("db") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if validate_profile_db_name(name).is_err()
                || provider.catalog.lock().unwrap().get(name).is_some()
            {
                continue;
            }
            dbs.push(json!({
                "db": name,
                "source": "profile",
                "mode": "rw",
                "verbs": ["read", "write"],
            }));
        }
    }

    Ok(ProviderOutput::ResultSet(
        json!({ "agent": agent, "databases": dbs }),
    ))
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
    // With a table name: that table's DDL. Without: every table's DDL — the
    // discovery path for catalog databases whose schema the agent has never seen.
    match task.graph_id.as_deref() {
        Some(table) => {
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
        None => {
            let mut stmt = conn
                .prepare("SELECT name, sql FROM sqlite_master WHERE type='table' ORDER BY name")?;
            let rows: Vec<Value> = stmt
                .query_map([], |row| {
                    Ok(json!({
                        "table": row.get::<_, String>(0)?,
                        "schema_sql": row.get::<_, Option<String>>(1)?,
                    }))
                })?
                .filter_map(|r| r.ok())
                .collect();
            Ok(ProviderOutput::ResultSet(json!({ "tables": rows })))
        }
    }
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
