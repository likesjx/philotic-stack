//! Structured query builder — `table.build`.
//!
//! Compiles a JSON spec into a parameterized single-statement SELECT so agents
//! never hand the runner raw SQL on the common path. Identifiers are validated,
//! operators come from a whitelist, and every value binds as a parameter. The
//! compiled SQL is returned alongside the rows for auditability.
//!
//! Spec shape (all keys except `table` optional):
//! ```json
//! {
//!   "table": "message",            // or {"table": "message", "as": "m"}
//!   "columns": ["m.text", "m.date"],
//!   "joins": [{"db": "contacts", "table": "handle", "as": "h",
//!              "type": "left", "on": {"left": "m.handle_id", "right": "h.ROWID"}}],
//!   "where": [{"column": "h.id", "op": "=", "value": "+15551234567"},
//!             {"column": "m.date", "op": ">", "value": 700000000}],
//!   "order_by": [{"column": "m.date", "dir": "desc"}],
//!   "limit": 50,
//!   "offset": 0
//! }
//! ```
//! `joins[].db` references another catalog database; the provider attaches it
//! read-only under that alias before the query runs.

use anyhow::{Result, bail};
use serde_json::Value;

pub struct BuiltQuery {
    pub sql: String,
    pub params: Vec<Value>,
    /// Catalog databases referenced via `joins[].db` — the provider must
    /// attach these (read-only) before executing.
    pub attach_dbs: Vec<String>,
}

/// A bare or dot-qualified identifier: `text`, `m.text`, `contacts.handle`.
fn validate_qualified(name: &str) -> Result<()> {
    let parts: Vec<&str> = name.split('.').collect();
    if parts.is_empty() || parts.len() > 3 {
        bail!("invalid identifier: {name:?}");
    }
    for part in parts {
        if part.is_empty()
            || !part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            bail!("invalid identifier: {name:?}");
        }
    }
    Ok(())
}

fn table_ref(v: &Value, attach_dbs: &mut Vec<String>) -> Result<String> {
    let (db, table, alias) = match v {
        Value::String(s) => (None, s.as_str(), None),
        Value::Object(o) => (
            o.get("db").and_then(Value::as_str),
            o.get("table")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("join/table object requires \"table\""))?,
            o.get("as").and_then(Value::as_str),
        ),
        _ => bail!("table reference must be a string or object"),
    };
    validate_qualified(table)?;
    let mut out = String::new();
    if let Some(db) = db {
        validate_qualified(db)?;
        if !attach_dbs.contains(&db.to_string()) {
            attach_dbs.push(db.to_string());
        }
        out.push_str(db);
        out.push('.');
    }
    out.push_str(table);
    if let Some(alias) = alias {
        validate_qualified(alias)?;
        out.push_str(" AS ");
        out.push_str(alias);
    }
    Ok(out)
}

fn compile_where(clauses: &[Value], params: &mut Vec<Value>) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    for clause in clauses {
        let obj = clause
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("where clause must be an object"))?;
        let column = obj
            .get("column")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("where clause requires \"column\""))?;
        validate_qualified(column)?;
        let op = obj.get("op").and_then(Value::as_str).unwrap_or("=");
        let part = match op {
            "=" | "!=" | "<" | "<=" | ">" | ">=" | "like" | "LIKE" => {
                let value = obj
                    .get("value")
                    .ok_or_else(|| anyhow::anyhow!("where op {op:?} requires \"value\""))?;
                params.push(value.clone());
                let sql_op = if op.eq_ignore_ascii_case("like") {
                    "LIKE"
                } else {
                    op
                };
                format!("{column} {sql_op} ?{}", params.len())
            }
            "in" | "IN" => {
                let values = obj.get("value").and_then(Value::as_array).ok_or_else(|| {
                    anyhow::anyhow!("where op \"in\" requires an array \"value\"")
                })?;
                if values.is_empty() {
                    bail!("where op \"in\" requires a non-empty array");
                }
                let mut placeholders: Vec<String> = Vec::new();
                for v in values {
                    params.push(v.clone());
                    placeholders.push(format!("?{}", params.len()));
                }
                format!("{column} IN ({})", placeholders.join(", "))
            }
            "is_null" => format!("{column} IS NULL"),
            "not_null" => format!("{column} IS NOT NULL"),
            other => bail!("unsupported where op: {other:?}"),
        };
        parts.push(part);
    }
    Ok(parts.join(" AND "))
}

pub fn build_query(spec: &Value) -> Result<BuiltQuery> {
    let obj = spec
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("table.build requires a JSON object spec in parameters"))?;

    let mut attach_dbs: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    // FROM
    let table_value = obj
        .get("table")
        .ok_or_else(|| anyhow::anyhow!("table.build requires \"table\""))?;
    let from = table_ref(table_value, &mut attach_dbs)?;

    // SELECT list
    let columns = match obj.get("columns").and_then(Value::as_array) {
        None => "*".to_string(),
        Some(cols) if cols.is_empty() => "*".to_string(),
        Some(cols) => {
            let mut out: Vec<String> = Vec::new();
            for col in cols {
                let name = col
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("columns entries must be strings"))?;
                if name == "*" {
                    out.push("*".to_string());
                } else {
                    validate_qualified(name)?;
                    out.push(name.to_string());
                }
            }
            out.join(", ")
        }
    };

    let mut sql = format!("SELECT {columns} FROM {from}");

    // JOINs
    if let Some(joins) = obj.get("joins").and_then(Value::as_array) {
        for join in joins {
            let jobj = join
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("joins entries must be objects"))?;
            let kind = match jobj.get("type").and_then(Value::as_str).unwrap_or("inner") {
                "inner" => "JOIN",
                "left" => "LEFT JOIN",
                other => bail!("unsupported join type: {other:?} (inner|left)"),
            };
            let target = table_ref(join, &mut attach_dbs)?;
            let on = jobj
                .get("on")
                .and_then(Value::as_object)
                .ok_or_else(|| anyhow::anyhow!("join requires \"on\": {{left, right}}"))?;
            let left = on
                .get("left")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("join on requires \"left\""))?;
            let right = on
                .get("right")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("join on requires \"right\""))?;
            validate_qualified(left)?;
            validate_qualified(right)?;
            sql.push_str(&format!(" {kind} {target} ON {left} = {right}"));
        }
    }

    // WHERE
    if let Some(clauses) = obj.get("where").and_then(Value::as_array) {
        if !clauses.is_empty() {
            let where_sql = compile_where(clauses, &mut params)?;
            sql.push_str(" WHERE ");
            sql.push_str(&where_sql);
        }
    }

    // ORDER BY
    if let Some(order) = obj.get("order_by").and_then(Value::as_array) {
        let mut parts: Vec<String> = Vec::new();
        for entry in order {
            let oobj = entry
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("order_by entries must be objects"))?;
            let column = oobj
                .get("column")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("order_by requires \"column\""))?;
            validate_qualified(column)?;
            let dir = match oobj.get("dir").and_then(Value::as_str).unwrap_or("asc") {
                "asc" | "ASC" => "ASC",
                "desc" | "DESC" => "DESC",
                other => bail!("unsupported order dir: {other:?}"),
            };
            parts.push(format!("{column} {dir}"));
        }
        if !parts.is_empty() {
            sql.push_str(" ORDER BY ");
            sql.push_str(&parts.join(", "));
        }
    }

    // LIMIT / OFFSET — always bounded.
    let limit = obj.get("limit").and_then(Value::as_u64).unwrap_or(200);
    sql.push_str(&format!(" LIMIT {limit}"));
    if let Some(offset) = obj.get("offset").and_then(Value::as_u64) {
        sql.push_str(&format!(" OFFSET {offset}"));
    }

    Ok(BuiltQuery {
        sql,
        params,
        attach_dbs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn simple_select() {
        let q = build_query(&json!({"table": "items"})).unwrap();
        assert_eq!(q.sql, "SELECT * FROM items LIMIT 200");
        assert!(q.params.is_empty());
        assert!(q.attach_dbs.is_empty());
    }

    #[test]
    fn where_order_limit() {
        let q = build_query(&json!({
            "table": "signals",
            "columns": ["provider", "latency_ms"],
            "where": [{"column": "provider", "op": "=", "value": "gemini"},
                       {"column": "latency_ms", "op": ">", "value": 100}],
            "order_by": [{"column": "latency_ms", "dir": "desc"}],
            "limit": 5
        }))
        .unwrap();
        assert_eq!(
            q.sql,
            "SELECT provider, latency_ms FROM signals WHERE provider = ?1 AND latency_ms > ?2 ORDER BY latency_ms DESC LIMIT 5"
        );
        assert_eq!(q.params, vec![json!("gemini"), json!(100)]);
    }

    #[test]
    fn cross_db_join_collects_attach() {
        let q = build_query(&json!({
            "table": {"table": "message", "as": "m"},
            "joins": [{"db": "contacts", "table": "handle", "as": "h",
                        "type": "left", "on": {"left": "m.handle_id", "right": "h.ROWID"}}]
        }))
        .unwrap();
        assert!(
            q.sql
                .contains("LEFT JOIN contacts.handle AS h ON m.handle_id = h.ROWID")
        );
        assert_eq!(q.attach_dbs, vec!["contacts".to_string()]);
    }

    #[test]
    fn rejects_identifier_injection() {
        assert!(build_query(&json!({"table": "items; DROP TABLE x"})).is_err());
        assert!(
            build_query(&json!({"table": "items", "where": [{"column": "1=1 --", "value": 1}]}))
                .is_err()
        );
        assert!(
            build_query(
                &json!({"table": "items", "order_by": [{"column": "x", "dir": "asc; --"}]})
            )
            .is_err()
        );
    }

    #[test]
    fn in_clause_binds_each_value() {
        let q = build_query(&json!({
            "table": "t",
            "where": [{"column": "id", "op": "in", "value": ["a", "b", "c"]}]
        }))
        .unwrap();
        assert!(q.sql.contains("id IN (?1, ?2, ?3)"));
        assert_eq!(q.params.len(), 3);
    }
}
