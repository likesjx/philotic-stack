use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection};
use ulid::Ulid;

use crate::access::{resolve_edge_visibility, resolve_node_visibility};
use crate::graph::{
    ColumnSpec, Edge, EdgeFilter, EdgeInput, GraphMeta, GraphSchema, GraphSpec, Identity, Node,
    NodeFilter, NodeInput, Row, RowInput, RowQuery, TableMeta, TableSpec, TraversalDirection,
    TraversalQuery, TraversalResult,
};
use crate::store::{GraphStore, GraphTableStore, TableStore};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn new_ulid() -> String {
    Ulid::new().to_string()
}

// ── SqliteGraphStore ──────────────────────────────────────────────────────────

pub struct SqliteGraphStore {
    conn: Mutex<Connection>,
}

impl SqliteGraphStore {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA_SQL)?;
        // Additive migrations for existing databases.
        let _ = conn.execute_batch(
            "ALTER TABLE nodes ADD COLUMN table_ref TEXT;
             CREATE INDEX IF NOT EXISTS idx_nodes_table_ref
                 ON nodes(table_ref) WHERE table_ref IS NOT NULL AND deleted_at IS NULL;",
        );
        Ok(())
    }
}

// ── Schema SQL ────────────────────────────────────────────────────────────────

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS graphs (
    graph_id           TEXT NOT NULL PRIMARY KEY,
    name               TEXT NOT NULL UNIQUE,
    description        TEXT,
    schema_json        TEXT NOT NULL DEFAULT '{"node_types":[],"edge_types":[],"strict":false}',
    default_visibility TEXT NOT NULL DEFAULT 'private',
    creator            TEXT NOT NULL DEFAULT '',
    created_at         INTEGER NOT NULL,
    updated_at         INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS nodes (
    node_id         TEXT NOT NULL,
    graph_id        TEXT NOT NULL REFERENCES graphs(graph_id),
    node_type       TEXT NOT NULL DEFAULT '',
    label           TEXT NOT NULL DEFAULT '',
    content_json    TEXT NOT NULL DEFAULT '{}',
    tags_json       TEXT NOT NULL DEFAULT '[]',
    visibility_json TEXT NOT NULL DEFAULT '[]',
    creator         TEXT NOT NULL DEFAULT '',
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    deleted_at      INTEGER,
    PRIMARY KEY (graph_id, node_id)
);

CREATE INDEX IF NOT EXISTS idx_nodes_graph_type
    ON nodes(graph_id, node_type)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_nodes_creator
    ON nodes(graph_id, creator)
    WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS edges (
    edge_id         TEXT NOT NULL,
    graph_id        TEXT NOT NULL REFERENCES graphs(graph_id),
    from_node_id    TEXT NOT NULL,
    to_node_id      TEXT NOT NULL,
    edge_type       TEXT NOT NULL DEFAULT '',
    label           TEXT,
    content_json    TEXT NOT NULL DEFAULT '{}',
    visibility_json TEXT NOT NULL DEFAULT '[]',
    creator         TEXT NOT NULL DEFAULT '',
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    deleted_at      INTEGER,
    PRIMARY KEY (graph_id, edge_id),
    FOREIGN KEY (graph_id, from_node_id) REFERENCES nodes(graph_id, node_id),
    FOREIGN KEY (graph_id, to_node_id)   REFERENCES nodes(graph_id, node_id)
);

CREATE INDEX IF NOT EXISTS idx_edges_from
    ON edges(graph_id, from_node_id)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_edges_to
    ON edges(graph_id, to_node_id)
    WHERE deleted_at IS NULL;

CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
    node_id,
    graph_id,
    label,
    content_json,
    tokenize = 'porter ascii'
);

CREATE TABLE IF NOT EXISTS tables (
    table_id     TEXT NOT NULL PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE,
    description  TEXT,
    columns_json TEXT NOT NULL DEFAULT '[]',
    graph_id     TEXT REFERENCES graphs(graph_id),
    creator      TEXT NOT NULL DEFAULT '',
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_tables_graph
    ON tables(graph_id)
    WHERE graph_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS table_rows (
    row_id     TEXT NOT NULL PRIMARY KEY,
    table_id   TEXT NOT NULL REFERENCES tables(table_id),
    data_json  TEXT NOT NULL DEFAULT '{}',
    creator    TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_table_rows_table
    ON table_rows(table_id)
    WHERE deleted_at IS NULL;
"#;

// ── Row deserialization helpers ───────────────────────────────────────────────

fn row_to_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<Node> {
    let content_str: String = row.get("content_json")?;
    let tags_str: String = row.get("tags_json")?;
    let vis_str: String = row.get("visibility_json")?;

    Ok(Node {
        node_id: row.get("node_id")?,
        graph_id: row.get("graph_id")?,
        node_type: row.get("node_type")?,
        label: row.get("label")?,
        content: serde_json::from_str(&content_str).unwrap_or(serde_json::Value::Null),
        tags: serde_json::from_str(&tags_str).unwrap_or_default(),
        visibility: serde_json::from_str(&vis_str).unwrap_or_default(),
        creator: row.get("creator")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        table_ref: row.get("table_ref").ok().flatten(),
    })
}

fn row_to_edge(row: &rusqlite::Row<'_>) -> rusqlite::Result<Edge> {
    let content_str: String = row.get("content_json")?;
    let vis_str: String = row.get("visibility_json")?;

    Ok(Edge {
        edge_id: row.get("edge_id")?,
        graph_id: row.get("graph_id")?,
        from_node_id: row.get("from_node_id")?,
        to_node_id: row.get("to_node_id")?,
        edge_type: row.get("edge_type")?,
        label: row.get("label")?,
        content: serde_json::from_str(&content_str).unwrap_or(serde_json::Value::Null),
        visibility: serde_json::from_str(&vis_str).unwrap_or_default(),
        creator: row.get("creator")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

// ── Schema validation helpers ─────────────────────────────────────────────────

fn validate_node_type(schema: &GraphSchema, node_type: &str) -> Result<()> {
    if schema.strict && !schema.node_types.iter().any(|t| t.name == node_type) {
        bail!(
            "node type '{}' is not defined in graph schema (strict mode)",
            node_type
        );
    }
    Ok(())
}

fn validate_edge_type(schema: &GraphSchema, edge_type: &str) -> Result<()> {
    if schema.strict && !schema.edge_types.iter().any(|t| t.name == edge_type) {
        bail!(
            "edge type '{}' is not defined in graph schema (strict mode)",
            edge_type
        );
    }
    Ok(())
}

// ── GraphStore implementation ─────────────────────────────────────────────────

impl GraphStore for SqliteGraphStore {
    fn create_graph(&self, spec: GraphSpec) -> Result<String> {
        let graph_id = new_ulid();
        let now = now_ms();
        let schema_json = serde_json::to_string(&spec.schema)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO graphs (graph_id, name, description, schema_json, default_visibility, creator, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                graph_id,
                spec.name,
                spec.description,
                schema_json,
                spec.default_visibility,
                spec.creator,
                now,
                now,
            ],
        )?;
        Ok(graph_id)
    }

    fn get_graph(&self, graph_id: &str) -> Result<Option<GraphMeta>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT graph_id, name, description, schema_json, default_visibility, creator, created_at, updated_at
             FROM graphs WHERE graph_id = ?1",
        )?;
        let mut rows = stmt.query(params![graph_id])?;
        if let Some(row) = rows.next()? {
            let schema_json: String = row.get("schema_json")?;
            let schema: GraphSchema = serde_json::from_str(&schema_json).unwrap_or_default();
            Ok(Some(GraphMeta {
                graph_id: row.get("graph_id")?,
                name: row.get("name")?,
                description: row.get("description")?,
                schema,
                default_visibility: row.get("default_visibility")?,
                creator: row.get("creator")?,
                created_at: row.get("created_at")?,
                updated_at: row.get("updated_at")?,
            }))
        } else {
            Ok(None)
        }
    }

    fn list_graphs(&self) -> Result<Vec<GraphMeta>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT graph_id, name, description, schema_json, default_visibility, creator, created_at, updated_at
             FROM graphs ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            let schema_json: String = row.get("schema_json")?;
            let schema: GraphSchema = serde_json::from_str(&schema_json).unwrap_or_default();
            Ok(GraphMeta {
                graph_id: row.get("graph_id")?,
                name: row.get("name")?,
                description: row.get("description")?,
                schema,
                default_visibility: row.get("default_visibility")?,
                creator: row.get("creator")?,
                created_at: row.get("created_at")?,
                updated_at: row.get("updated_at")?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn update_schema(&self, graph_id: &str, new_schema: GraphSchema) -> Result<()> {
        let meta = self
            .get_graph(graph_id)?
            .ok_or_else(|| anyhow!("graph '{}' not found", graph_id))?;

        // Additive-only: reject removal of node types that have live nodes.
        let existing_node_type_names: HashSet<_> = meta
            .schema
            .node_types
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        let new_node_type_names: HashSet<_> = new_schema
            .node_types
            .iter()
            .map(|t| t.name.as_str())
            .collect();

        for removed in existing_node_type_names.difference(&new_node_type_names) {
            let conn = self.conn.lock().unwrap();
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM nodes WHERE graph_id = ?1 AND node_type = ?2 AND deleted_at IS NULL",
                params![graph_id, removed],
                |r| r.get(0),
            )?;
            if count > 0 {
                bail!(
                    "cannot remove node type '{}': {} node(s) still use it",
                    removed,
                    count
                );
            }
        }

        let schema_json = serde_json::to_string(&new_schema)?;
        let now = now_ms();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE graphs SET schema_json = ?1, updated_at = ?2 WHERE graph_id = ?3",
            params![schema_json, now, graph_id],
        )?;
        Ok(())
    }

    fn upsert_node(&self, graph_id: &str, input: NodeInput) -> Result<String> {
        let meta = self
            .get_graph(graph_id)?
            .ok_or_else(|| anyhow!("graph '{}' not found", graph_id))?;

        validate_node_type(&meta.schema, &input.node_type)?;

        let node_id = input.node_id.unwrap_or_else(new_ulid);
        let now = now_ms();
        let content_json = serde_json::to_string(&input.content)?;
        let tags_json = serde_json::to_string(&input.tags)?;
        let vis_json = serde_json::to_string(&input.visibility)?;

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO nodes (node_id, graph_id, node_type, label, content_json, tags_json, visibility_json, creator, table_ref, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(graph_id, node_id) DO UPDATE SET
               node_type       = excluded.node_type,
               label           = excluded.label,
               content_json    = excluded.content_json,
               tags_json       = excluded.tags_json,
               visibility_json = excluded.visibility_json,
               table_ref       = excluded.table_ref,
               updated_at      = excluded.updated_at,
               deleted_at      = NULL",
            params![
                node_id, graph_id, input.node_type, input.label,
                content_json, tags_json, vis_json, input.creator, input.table_ref, now, now,
            ],
        )?;

        // Sync FTS index.
        conn.execute(
            "INSERT OR REPLACE INTO nodes_fts (node_id, graph_id, label, content_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![node_id, graph_id, input.label, content_json],
        )?;

        Ok(node_id)
    }

    fn get_node(&self, graph_id: &str, node_id: &str, identity: &Identity) -> Result<Option<Node>> {
        let meta = match self.get_graph(graph_id)? {
            Some(m) => m,
            None => return Ok(None),
        };
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT node_id, graph_id, node_type, label, content_json, tags_json, visibility_json, creator, table_ref, created_at, updated_at
             FROM nodes WHERE graph_id = ?1 AND node_id = ?2 AND deleted_at IS NULL",
        )?;
        let mut rows = stmt.query(params![graph_id, node_id])?;
        if let Some(row) = rows.next()? {
            let node = row_to_node(row)?;
            if resolve_node_visibility(&node.visibility, &meta.default_visibility, identity) {
                Ok(Some(node))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    fn list_nodes(
        &self,
        graph_id: &str,
        filter: &NodeFilter,
        identity: &Identity,
    ) -> Result<Vec<Node>> {
        let meta = match self.get_graph(graph_id)? {
            Some(m) => m,
            None => return Ok(vec![]),
        };

        let conn = self.conn.lock().unwrap();

        // Build a dynamic query. We always filter by graph_id and deleted_at.
        let mut sql = String::from(
            "SELECT node_id, graph_id, node_type, label, content_json, tags_json, visibility_json, creator, table_ref, created_at, updated_at
             FROM nodes WHERE graph_id = ?1 AND deleted_at IS NULL",
        );
        let mut param_values: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(graph_id.to_string())];
        let mut param_idx = 2usize;

        if let Some(nt) = &filter.node_type {
            sql.push_str(&format!(" AND node_type = ?{}", param_idx));
            param_values.push(Box::new(nt.clone()));
            param_idx += 1;
        }
        if let Some(creator) = &filter.creator {
            sql.push_str(&format!(" AND creator = ?{}", param_idx));
            param_values.push(Box::new(creator.clone()));
            // param_idx += 1; // unused after this
        }

        let mut stmt = conn.prepare(&sql)?;
        let params_ref: Vec<&dyn rusqlite::ToSql> =
            param_values.iter().map(|b| b.as_ref()).collect();

        let all_nodes = stmt
            .query_map(params_ref.as_slice(), row_to_node)?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        // Apply in-memory: visibility + optional tag filter (tag filter requires JSON parsing).
        let nodes = all_nodes
            .into_iter()
            .filter(|n| {
                // Visibility gate.
                if !resolve_node_visibility(&n.visibility, &meta.default_visibility, identity) {
                    return false;
                }
                // Tag filter: node must contain ALL requested tags.
                if let Some(required_tags) = &filter.tags {
                    for required in required_tags {
                        if !n.tags.contains(required) {
                            return false;
                        }
                    }
                }
                true
            })
            .collect();

        Ok(nodes)
    }

    fn delete_node(&self, graph_id: &str, node_id: &str) -> Result<()> {
        let now = now_ms();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE nodes SET deleted_at = ?1 WHERE graph_id = ?2 AND node_id = ?3",
            params![now, graph_id, node_id],
        )?;
        // Remove from FTS.
        conn.execute(
            "DELETE FROM nodes_fts WHERE graph_id = ?1 AND node_id = ?2",
            params![graph_id, node_id],
        )?;
        Ok(())
    }

    fn upsert_edge(&self, graph_id: &str, input: EdgeInput) -> Result<String> {
        let meta = self
            .get_graph(graph_id)?
            .ok_or_else(|| anyhow!("graph '{}' not found", graph_id))?;

        validate_edge_type(&meta.schema, &input.edge_type)?;

        // Verify both endpoint nodes exist (not deleted).
        {
            let conn = self.conn.lock().unwrap();
            let from_exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM nodes WHERE graph_id = ?1 AND node_id = ?2 AND deleted_at IS NULL)",
                params![graph_id, input.from_node_id],
                |r| r.get(0),
            )?;
            if !from_exists {
                bail!(
                    "from_node_id '{}' not found in graph '{}'",
                    input.from_node_id,
                    graph_id
                );
            }
            let to_exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM nodes WHERE graph_id = ?1 AND node_id = ?2 AND deleted_at IS NULL)",
                params![graph_id, input.to_node_id],
                |r| r.get(0),
            )?;
            if !to_exists {
                bail!(
                    "to_node_id '{}' not found in graph '{}'",
                    input.to_node_id,
                    graph_id
                );
            }
        }

        let edge_id = input.edge_id.unwrap_or_else(new_ulid);
        let now = now_ms();
        let content_json = serde_json::to_string(&input.content)?;
        let vis_json = serde_json::to_string(&input.visibility)?;

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO edges (edge_id, graph_id, from_node_id, to_node_id, edge_type, label, content_json, visibility_json, creator, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(graph_id, edge_id) DO UPDATE SET
               edge_type       = excluded.edge_type,
               label           = excluded.label,
               content_json    = excluded.content_json,
               visibility_json = excluded.visibility_json,
               updated_at      = excluded.updated_at,
               deleted_at      = NULL",
            params![
                edge_id, graph_id, input.from_node_id, input.to_node_id,
                input.edge_type, input.label, content_json, vis_json,
                input.creator, now, now,
            ],
        )?;
        Ok(edge_id)
    }

    fn get_edge(&self, graph_id: &str, edge_id: &str, identity: &Identity) -> Result<Option<Edge>> {
        let meta = match self.get_graph(graph_id)? {
            Some(m) => m,
            None => return Ok(None),
        };
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT edge_id, graph_id, from_node_id, to_node_id, edge_type, label, content_json, visibility_json, creator, created_at, updated_at
             FROM edges WHERE graph_id = ?1 AND edge_id = ?2 AND deleted_at IS NULL",
        )?;
        let mut rows = stmt.query(params![graph_id, edge_id])?;
        if let Some(row) = rows.next()? {
            let edge = row_to_edge(row)?;
            // Check both endpoint visibilities and the edge's own visibility.
            let from_vis = self.node_is_visible_locked(
                &conn,
                graph_id,
                &edge.from_node_id,
                &meta.default_visibility,
                identity,
            );
            let to_vis = self.node_is_visible_locked(
                &conn,
                graph_id,
                &edge.to_node_id,
                &meta.default_visibility,
                identity,
            );
            if resolve_edge_visibility(
                &edge.visibility,
                &meta.default_visibility,
                from_vis,
                to_vis,
                identity,
            ) {
                Ok(Some(edge))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    fn list_edges(
        &self,
        graph_id: &str,
        filter: &EdgeFilter,
        identity: &Identity,
    ) -> Result<Vec<Edge>> {
        let meta = match self.get_graph(graph_id)? {
            Some(m) => m,
            None => return Ok(vec![]),
        };
        let conn = self.conn.lock().unwrap();

        let mut sql = String::from(
            "SELECT edge_id, graph_id, from_node_id, to_node_id, edge_type, label, content_json, visibility_json, creator, created_at, updated_at
             FROM edges WHERE graph_id = ?1 AND deleted_at IS NULL",
        );
        let mut param_values: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(graph_id.to_string())];
        let mut param_idx = 2usize;

        if let Some(from) = &filter.from_node_id {
            sql.push_str(&format!(" AND from_node_id = ?{}", param_idx));
            param_values.push(Box::new(from.clone()));
            param_idx += 1;
        }
        if let Some(to) = &filter.to_node_id {
            sql.push_str(&format!(" AND to_node_id = ?{}", param_idx));
            param_values.push(Box::new(to.clone()));
            param_idx += 1;
        }
        if let Some(et) = &filter.edge_type {
            sql.push_str(&format!(" AND edge_type = ?{}", param_idx));
            param_values.push(Box::new(et.clone()));
            param_idx += 1;
        }
        if let Some(creator) = &filter.creator {
            sql.push_str(&format!(" AND creator = ?{}", param_idx));
            param_values.push(Box::new(creator.clone()));
            // param_idx += 1;
        }

        let mut stmt = conn.prepare(&sql)?;
        let params_ref: Vec<&dyn rusqlite::ToSql> =
            param_values.iter().map(|b| b.as_ref()).collect();
        let all_edges = stmt
            .query_map(params_ref.as_slice(), row_to_edge)?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        // Cache node visibility lookups to avoid redundant queries.
        let mut vis_cache: HashMap<String, bool> = HashMap::new();
        let mut node_visible = |node_id: &str| -> bool {
            if let Some(&v) = vis_cache.get(node_id) {
                return v;
            }
            let v = self.node_is_visible_locked(
                &conn,
                graph_id,
                node_id,
                &meta.default_visibility,
                identity,
            );
            vis_cache.insert(node_id.to_string(), v);
            v
        };

        let edges = all_edges
            .into_iter()
            .filter(|e| {
                let from_vis = node_visible(&e.from_node_id);
                let to_vis = node_visible(&e.to_node_id);
                resolve_edge_visibility(
                    &e.visibility,
                    &meta.default_visibility,
                    from_vis,
                    to_vis,
                    identity,
                )
            })
            .collect();

        Ok(edges)
    }

    fn delete_edge(&self, graph_id: &str, edge_id: &str) -> Result<()> {
        let now = now_ms();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE edges SET deleted_at = ?1 WHERE graph_id = ?2 AND edge_id = ?3",
            params![now, graph_id, edge_id],
        )?;
        Ok(())
    }

    fn traverse(
        &self,
        graph_id: &str,
        query: &TraversalQuery,
        identity: &Identity,
    ) -> Result<TraversalResult> {
        let meta = self
            .get_graph(graph_id)?
            .ok_or_else(|| anyhow!("graph '{}' not found", graph_id))?;

        let mut visited_nodes: HashSet<String> = HashSet::new();
        let mut result_nodes: Vec<crate::graph::Node> = Vec::new();
        let mut result_edges: Vec<Edge> = Vec::new();

        // BFS queue: (node_id, depth)
        let mut queue: VecDeque<(String, u32)> = VecDeque::new();
        queue.push_back((query.start_node_id.clone(), 0));

        while let Some((current_id, depth)) = queue.pop_front() {
            if visited_nodes.contains(&current_id) {
                continue;
            }

            // Fetch and visibility-check the node.
            let node = match self.get_node(graph_id, &current_id, identity)? {
                Some(n) => n,
                None => continue, // not visible or deleted
            };

            visited_nodes.insert(current_id.clone());
            result_nodes.push(node);

            if depth >= query.max_depth {
                continue;
            }

            // Fetch adjacent edges according to the traversal direction.
            let conn = self.conn.lock().unwrap();
            let mut candidate_edges: Vec<Edge> = Vec::new();

            match &query.direction {
                TraversalDirection::Outbound | TraversalDirection::Both => {
                    let edges = Self::fetch_edges_from_locked(
                        &conn,
                        graph_id,
                        &current_id,
                        query.edge_types.as_deref(),
                    )?;
                    candidate_edges.extend(edges);
                }
                _ => {}
            }
            match &query.direction {
                TraversalDirection::Inbound | TraversalDirection::Both => {
                    let edges = Self::fetch_edges_to_locked(
                        &conn,
                        graph_id,
                        &current_id,
                        query.edge_types.as_deref(),
                    )?;
                    candidate_edges.extend(edges);
                }
                _ => {}
            }
            drop(conn);

            for edge in candidate_edges {
                let from_vis = self.node_raw_visible(
                    graph_id,
                    &edge.from_node_id,
                    &meta.default_visibility,
                    identity,
                );
                let to_vis = self.node_raw_visible(
                    graph_id,
                    &edge.to_node_id,
                    &meta.default_visibility,
                    identity,
                );
                if !resolve_edge_visibility(
                    &edge.visibility,
                    &meta.default_visibility,
                    from_vis,
                    to_vis,
                    identity,
                ) {
                    continue;
                }

                let next_id = match &query.direction {
                    TraversalDirection::Outbound => edge.to_node_id.clone(),
                    TraversalDirection::Inbound => edge.from_node_id.clone(),
                    TraversalDirection::Both => {
                        if edge.from_node_id == current_id {
                            edge.to_node_id.clone()
                        } else {
                            edge.from_node_id.clone()
                        }
                    }
                };

                if !visited_nodes.contains(&next_id) {
                    queue.push_back((next_id, depth + 1));
                }
                result_edges.push(edge);
            }
        }

        Ok(TraversalResult {
            nodes: result_nodes,
            edges: result_edges,
        })
    }

    fn search_nodes(
        &self,
        graph_id: &str,
        query: &str,
        identity: &Identity,
    ) -> Result<Vec<crate::graph::Node>> {
        let meta = match self.get_graph(graph_id)? {
            Some(m) => m,
            None => return Ok(vec![]),
        };
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT n.node_id, n.graph_id, n.node_type, n.label, n.content_json, n.tags_json, n.visibility_json, n.creator, n.table_ref, n.created_at, n.updated_at
             FROM nodes n
             JOIN nodes_fts f ON n.node_id = f.node_id AND n.graph_id = f.graph_id
             WHERE f.nodes_fts MATCH ?1
               AND n.graph_id = ?2
               AND n.deleted_at IS NULL",
        )?;
        let all_nodes = stmt
            .query_map(params![query, graph_id], row_to_node)?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let nodes = all_nodes
            .into_iter()
            .filter(|n| resolve_node_visibility(&n.visibility, &meta.default_visibility, identity))
            .collect();

        Ok(nodes)
    }
}

// ── Internal helpers (not part of the trait) ──────────────────────────────────

impl SqliteGraphStore {
    /// Check node visibility using an already-held connection lock.
    fn node_is_visible_locked(
        &self,
        conn: &Connection,
        graph_id: &str,
        node_id: &str,
        default_visibility: &str,
        identity: &Identity,
    ) -> bool {
        let result: rusqlite::Result<String> = conn.query_row(
            "SELECT visibility_json FROM nodes WHERE graph_id = ?1 AND node_id = ?2 AND deleted_at IS NULL",
            params![graph_id, node_id],
            |r| r.get(0),
        );
        match result {
            Ok(vis_json) => {
                let vis: Vec<String> = serde_json::from_str(&vis_json).unwrap_or_default();
                resolve_node_visibility(&vis, default_visibility, identity)
            }
            Err(_) => false,
        }
    }

    /// Same check but acquires the lock itself (for use in traverse where conn is already dropped).
    fn node_raw_visible(
        &self,
        graph_id: &str,
        node_id: &str,
        default_visibility: &str,
        identity: &Identity,
    ) -> bool {
        let conn = self.conn.lock().unwrap();
        self.node_is_visible_locked(&conn, graph_id, node_id, default_visibility, identity)
    }

    fn fetch_edges_from_locked(
        conn: &Connection,
        graph_id: &str,
        from_node_id: &str,
        edge_type_filter: Option<&[String]>,
    ) -> Result<Vec<Edge>> {
        let base = "SELECT edge_id, graph_id, from_node_id, to_node_id, edge_type, label, content_json, visibility_json, creator, created_at, updated_at
                    FROM edges WHERE graph_id = ?1 AND from_node_id = ?2 AND deleted_at IS NULL";
        Self::fetch_edges_with_optional_type(conn, base, graph_id, from_node_id, edge_type_filter)
    }

    fn fetch_edges_to_locked(
        conn: &Connection,
        graph_id: &str,
        to_node_id: &str,
        edge_type_filter: Option<&[String]>,
    ) -> Result<Vec<Edge>> {
        let base = "SELECT edge_id, graph_id, from_node_id, to_node_id, edge_type, label, content_json, visibility_json, creator, created_at, updated_at
                    FROM edges WHERE graph_id = ?1 AND to_node_id = ?2 AND deleted_at IS NULL";
        Self::fetch_edges_with_optional_type(conn, base, graph_id, to_node_id, edge_type_filter)
    }

    fn fetch_edges_with_optional_type(
        conn: &Connection,
        base_sql: &str,
        graph_id: &str,
        node_id: &str,
        edge_type_filter: Option<&[String]>,
    ) -> Result<Vec<Edge>> {
        // If there's an edge type filter, we apply it in memory since SQLite
        // doesn't easily handle IN (?) for variable-length lists without extra work.
        let mut stmt = conn.prepare(base_sql)?;
        let all_edges = stmt
            .query_map(params![graph_id, node_id], row_to_edge)?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        if let Some(types) = edge_type_filter {
            Ok(all_edges
                .into_iter()
                .filter(|e| types.contains(&e.edge_type))
                .collect())
        } else {
            Ok(all_edges)
        }
    }
}

// ── TableStore implementation ──────────────────────────────────────────────────

fn row_to_table_meta(row: &rusqlite::Row<'_>) -> rusqlite::Result<TableMeta> {
    let cols_str: String = row.get("columns_json")?;
    Ok(TableMeta {
        table_id: row.get("table_id")?,
        name: row.get("name")?,
        description: row.get("description")?,
        columns: serde_json::from_str(&cols_str).unwrap_or_default(),
        graph_id: row.get("graph_id")?,
        creator: row.get("creator")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn row_to_table_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    let data_str: String = row.get("data_json")?;
    Ok(Row {
        row_id: row.get("row_id")?,
        table_id: row.get("table_id")?,
        data: serde_json::from_str(&data_str).unwrap_or(serde_json::Value::Null),
        creator: row.get("creator")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

impl TableStore for SqliteGraphStore {
    fn create_table(&self, spec: TableSpec) -> Result<String> {
        let table_id = new_ulid();
        let now = now_ms();
        let cols_json = serde_json::to_string(&spec.columns)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO tables (table_id, name, description, columns_json, graph_id, creator, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![table_id, spec.name, spec.description, cols_json, spec.graph_id, spec.creator, now, now],
        )?;
        Ok(table_id)
    }

    fn get_table(&self, table_id: &str) -> Result<Option<TableMeta>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT table_id, name, description, columns_json, graph_id, creator, created_at, updated_at
             FROM tables WHERE table_id = ?1",
        )?;
        let mut rows = stmt.query(params![table_id])?;
        Ok(rows.next()?.map(|r| row_to_table_meta(r)).transpose()?)
    }

    fn list_tables(&self, graph_id: Option<&str>) -> Result<Vec<TableMeta>> {
        let conn = self.conn.lock().unwrap();
        let (sql, param): (&str, Box<dyn rusqlite::ToSql>) = match graph_id {
            Some(gid) => (
                "SELECT table_id, name, description, columns_json, graph_id, creator, created_at, updated_at
                 FROM tables WHERE graph_id = ?1 ORDER BY created_at ASC",
                Box::new(gid.to_string()),
            ),
            None => (
                "SELECT table_id, name, description, columns_json, graph_id, creator, created_at, updated_at
                 FROM tables ORDER BY created_at ASC",
                Box::new(rusqlite::types::Null),
            ),
        };
        let mut stmt = conn.prepare(sql)?;
        match graph_id {
            Some(_) => {
                let rows = stmt
                    .query_map([param.as_ref()], row_to_table_meta)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            }
            None => {
                let rows = stmt
                    .query_map([], row_to_table_meta)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            }
        }
    }

    fn update_table(
        &self,
        table_id: &str,
        name: Option<String>,
        description: Option<String>,
        columns: Option<Vec<ColumnSpec>>,
    ) -> Result<()> {
        let meta = self
            .get_table(table_id)?
            .ok_or_else(|| anyhow!("table '{}' not found", table_id))?;

        let new_name = name.unwrap_or(meta.name);
        let new_description = description.or(meta.description);

        // Additive-only on columns: reject removal of any column that has data.
        let new_columns = if let Some(cols) = columns {
            let existing_names: HashSet<_> = meta.columns.iter().map(|c| c.name.as_str()).collect();
            let new_names: HashSet<_> = cols.iter().map(|c| c.name.as_str()).collect();
            for removed in existing_names.difference(&new_names) {
                // Check whether any row has this field populated.
                let conn = self.conn.lock().unwrap();
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM table_rows WHERE table_id = ?1 AND deleted_at IS NULL AND json_extract(data_json, ?) IS NOT NULL",
                    params![table_id, format!("$.{}", removed)],
                    |r| r.get(0),
                )?;
                if count > 0 {
                    bail!(
                        "cannot remove column '{}': {} row(s) have data for it",
                        removed,
                        count
                    );
                }
            }
            cols
        } else {
            meta.columns
        };

        let cols_json = serde_json::to_string(&new_columns)?;
        let now = now_ms();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE tables SET name = ?1, description = ?2, columns_json = ?3, updated_at = ?4 WHERE table_id = ?5",
            params![new_name, new_description, cols_json, now, table_id],
        )?;
        Ok(())
    }

    fn drop_table(&self, table_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM table_rows WHERE table_id = ?1",
            params![table_id],
        )?;
        conn.execute("DELETE FROM tables WHERE table_id = ?1", params![table_id])?;
        Ok(())
    }

    fn insert_row(&self, table_id: &str, input: RowInput) -> Result<String> {
        // Verify table exists.
        self.get_table(table_id)?
            .ok_or_else(|| anyhow!("table '{}' not found", table_id))?;

        let row_id = input.row_id.unwrap_or_else(new_ulid);
        let now = now_ms();
        let data_json = serde_json::to_string(&input.data)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO table_rows (row_id, table_id, data_json, creator, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(row_id) DO UPDATE SET
               data_json  = excluded.data_json,
               updated_at = excluded.updated_at,
               deleted_at = NULL",
            params![row_id, table_id, data_json, input.creator, now, now],
        )?;
        Ok(row_id)
    }

    fn get_row(&self, table_id: &str, row_id: &str) -> Result<Option<Row>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT row_id, table_id, data_json, creator, created_at, updated_at
             FROM table_rows WHERE table_id = ?1 AND row_id = ?2 AND deleted_at IS NULL",
        )?;
        let mut rows = stmt.query(params![table_id, row_id])?;
        Ok(rows.next()?.map(|r| row_to_table_row(r)).transpose()?)
    }

    fn update_row(&self, table_id: &str, row_id: &str, patch: serde_json::Value) -> Result<()> {
        let existing = self
            .get_row(table_id, row_id)?
            .ok_or_else(|| anyhow!("row '{}' not found in table '{}'", row_id, table_id))?;

        // Shallow merge: patch keys overwrite existing keys.
        let merged = match existing.data {
            serde_json::Value::Object(mut map) => {
                if let serde_json::Value::Object(patch_map) = patch {
                    for (k, v) in patch_map {
                        map.insert(k, v);
                    }
                }
                serde_json::Value::Object(map)
            }
            _ => patch,
        };
        let data_json = serde_json::to_string(&merged)?;
        let now = now_ms();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE table_rows SET data_json = ?1, updated_at = ?2 WHERE table_id = ?3 AND row_id = ?4",
            params![data_json, now, table_id, row_id],
        )?;
        drop(merged); // silence unused warning
        Ok(())
    }

    fn delete_row(&self, table_id: &str, row_id: &str) -> Result<()> {
        let now = now_ms();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE table_rows SET deleted_at = ?1 WHERE table_id = ?2 AND row_id = ?3",
            params![now, table_id, row_id],
        )?;
        Ok(())
    }

    fn query_rows(&self, table_id: &str, query: &RowQuery) -> Result<Vec<Row>> {
        let conn = self.conn.lock().unwrap();
        let offset = query.offset.unwrap_or(0) as i64;
        let limit = query.limit.map(|l| l as i64).unwrap_or(i64::MAX);

        let mut stmt = conn.prepare(
            "SELECT row_id, table_id, data_json, creator, created_at, updated_at
             FROM table_rows WHERE table_id = ?1 AND deleted_at IS NULL
             ORDER BY created_at ASC LIMIT ?2 OFFSET ?3",
        )?;
        let all_rows = stmt
            .query_map(params![table_id, limit, offset], row_to_table_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        // Apply in-memory equality filter.
        let rows = if let Some(filter) = &query.filter {
            if let serde_json::Value::Object(filter_map) = filter {
                all_rows
                    .into_iter()
                    .filter(|row| {
                        if let serde_json::Value::Object(data_map) = &row.data {
                            filter_map
                                .iter()
                                .all(|(k, v)| data_map.get(k).map_or(false, |dv| dv == v))
                        } else {
                            false
                        }
                    })
                    .collect()
            } else {
                all_rows
            }
        } else {
            all_rows
        };

        Ok(rows)
    }
}

impl GraphTableStore for SqliteGraphStore {}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::*;

    fn store() -> SqliteGraphStore {
        SqliteGraphStore::open_in_memory().expect("in-memory store")
    }

    fn public_graph(store: &SqliteGraphStore) -> String {
        store
            .create_graph(GraphSpec {
                name: "test-graph".into(),
                description: None,
                schema: GraphSchema {
                    node_types: vec![
                        NodeTypeSpec {
                            name: "Research".into(),
                            description: None,
                        },
                        NodeTypeSpec {
                            name: "Goal".into(),
                            description: None,
                        },
                    ],
                    edge_types: vec![EdgeTypeSpec {
                        name: "SUPPORTS".into(),
                        description: None,
                        allowed_from: None,
                        allowed_to: None,
                    }],
                    strict: true,
                },
                default_visibility: "public".into(),
                creator: "alice".into(),
            })
            .expect("create graph")
    }

    fn private_graph(store: &SqliteGraphStore) -> String {
        store
            .create_graph(GraphSpec {
                name: "private-graph".into(),
                description: None,
                schema: GraphSchema::default(),
                default_visibility: "private".into(),
                creator: "alice".into(),
            })
            .expect("create private graph")
    }

    fn alice() -> Identity {
        Identity::new("alice").with_roles(vec!["researcher".into()])
    }

    fn bob() -> Identity {
        Identity::new("bob")
    }

    // ── Graph CRUD ────────────────────────────────────────────────────────────

    #[test]
    fn create_and_get_graph() {
        let store = store();
        let gid = public_graph(&store);
        let meta = store.get_graph(&gid).unwrap().unwrap();
        assert_eq!(meta.name, "test-graph");
        assert_eq!(meta.default_visibility, "public");
        assert_eq!(meta.schema.node_types.len(), 2);
    }

    #[test]
    fn list_graphs_returns_all() {
        let store = store();
        let _ = public_graph(&store);
        let _ = private_graph(&store);
        let graphs = store.list_graphs().unwrap();
        assert_eq!(graphs.len(), 2);
    }

    // ── Node CRUD ─────────────────────────────────────────────────────────────

    #[test]
    fn upsert_and_get_node() {
        let store = store();
        let gid = public_graph(&store);
        let nid = store
            .upsert_node(
                &gid,
                NodeInput {
                    node_id: None,
                    node_type: "Research".into(),
                    label: "Finding Alpha".into(),
                    content: serde_json::json!({ "summary": "important" }),
                    tags: vec!["important".into()],
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                    table_ref: None,
                },
            )
            .unwrap();

        let node = store.get_node(&gid, &nid, &alice()).unwrap().unwrap();
        assert_eq!(node.label, "Finding Alpha");
        assert_eq!(node.node_type, "Research");
    }

    #[test]
    fn strict_schema_rejects_unknown_node_type() {
        let store = store();
        let gid = public_graph(&store);
        let result = store.upsert_node(
            &gid,
            NodeInput {
                node_id: None,
                node_type: "Unknown".into(),
                label: "oops".into(),
                content: serde_json::Value::Null,
                tags: vec![],
                visibility: vec![],
                creator: "alice".into(),
                table_ref: None,
            },
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("strict mode"));
    }

    #[test]
    fn node_upsert_is_idempotent() {
        let store = store();
        let gid = public_graph(&store);
        let nid = "fixed-id".to_string();
        let input = || NodeInput {
            node_id: Some(nid.clone()),
            node_type: "Research".into(),
            label: "First".into(),
            content: serde_json::Value::Null,
            tags: vec![],
            visibility: vec!["public".into()],
            creator: "alice".into(),
            table_ref: None,
        };
        store.upsert_node(&gid, input()).unwrap();
        let mut updated = input();
        updated.label = "Updated".into();
        store.upsert_node(&gid, updated).unwrap();

        let node = store.get_node(&gid, &nid, &alice()).unwrap().unwrap();
        assert_eq!(node.label, "Updated");
    }

    #[test]
    fn soft_delete_hides_node() {
        let store = store();
        let gid = public_graph(&store);
        let nid = store
            .upsert_node(
                &gid,
                NodeInput {
                    node_id: None,
                    node_type: "Research".into(),
                    label: "gone".into(),
                    content: serde_json::Value::Null,
                    tags: vec![],
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                    table_ref: None,
                },
            )
            .unwrap();
        store.delete_node(&gid, &nid).unwrap();
        assert!(store.get_node(&gid, &nid, &alice()).unwrap().is_none());
    }

    // ── Visibility ────────────────────────────────────────────────────────────

    #[test]
    fn private_node_hidden_from_other_identity() {
        let store = store();
        let gid = private_graph(&store);
        let nid = store
            .upsert_node(
                &gid,
                NodeInput {
                    node_id: None,
                    node_type: "".into(),
                    label: "secret".into(),
                    content: serde_json::Value::Null,
                    tags: vec![],
                    visibility: vec!["identity:alice".into()],
                    creator: "alice".into(),
                    table_ref: None,
                },
            )
            .unwrap();
        assert!(store.get_node(&gid, &nid, &alice()).unwrap().is_some());
        assert!(store.get_node(&gid, &nid, &bob()).unwrap().is_none());
    }

    #[test]
    fn role_visibility_permits_matching_role() {
        let store = store();
        let gid = private_graph(&store);
        let nid = store
            .upsert_node(
                &gid,
                NodeInput {
                    node_id: None,
                    node_type: "".into(),
                    label: "research-only".into(),
                    content: serde_json::Value::Null,
                    tags: vec![],
                    visibility: vec!["role:researcher".into()],
                    creator: "alice".into(),
                    table_ref: None,
                },
            )
            .unwrap();
        assert!(store.get_node(&gid, &nid, &alice()).unwrap().is_some()); // alice has researcher role
        assert!(store.get_node(&gid, &nid, &bob()).unwrap().is_none()); // bob does not
    }

    // ── Edge CRUD ─────────────────────────────────────────────────────────────

    #[test]
    fn upsert_and_get_edge() {
        let store = store();
        let gid = public_graph(&store);
        let n1 = store
            .upsert_node(
                &gid,
                NodeInput {
                    node_id: None,
                    node_type: "Research".into(),
                    label: "A".into(),
                    content: serde_json::Value::Null,
                    tags: vec![],
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                    table_ref: None,
                },
            )
            .unwrap();
        let n2 = store
            .upsert_node(
                &gid,
                NodeInput {
                    node_id: None,
                    node_type: "Goal".into(),
                    label: "B".into(),
                    content: serde_json::Value::Null,
                    tags: vec![],
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                    table_ref: None,
                },
            )
            .unwrap();
        let eid = store
            .upsert_edge(
                &gid,
                EdgeInput {
                    edge_id: None,
                    from_node_id: n1.clone(),
                    to_node_id: n2.clone(),
                    edge_type: "SUPPORTS".into(),
                    label: None,
                    content: serde_json::Value::Null,
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                },
            )
            .unwrap();

        let edge = store.get_edge(&gid, &eid, &alice()).unwrap().unwrap();
        assert_eq!(edge.from_node_id, n1);
        assert_eq!(edge.to_node_id, n2);
        assert_eq!(edge.edge_type, "SUPPORTS");
    }

    #[test]
    fn edge_hidden_when_endpoint_not_visible() {
        let store = store();
        let gid = private_graph(&store);
        let n1 = store
            .upsert_node(
                &gid,
                NodeInput {
                    node_id: None,
                    node_type: "".into(),
                    label: "A".into(),
                    content: serde_json::Value::Null,
                    tags: vec![],
                    visibility: vec!["identity:alice".into()], // alice only
                    creator: "alice".into(),
                    table_ref: None,
                },
            )
            .unwrap();
        let n2 = store
            .upsert_node(
                &gid,
                NodeInput {
                    node_id: None,
                    node_type: "".into(),
                    label: "B".into(),
                    content: serde_json::Value::Null,
                    tags: vec![],
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                    table_ref: None,
                },
            )
            .unwrap();
        let eid = store
            .upsert_edge(
                &gid,
                EdgeInput {
                    edge_id: None,
                    from_node_id: n1,
                    to_node_id: n2,
                    edge_type: "".into(),
                    label: None,
                    content: serde_json::Value::Null,
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                },
            )
            .unwrap();

        assert!(store.get_edge(&gid, &eid, &alice()).unwrap().is_some());
        assert!(store.get_edge(&gid, &eid, &bob()).unwrap().is_none()); // n1 hidden from bob
    }

    // ── Traversal ─────────────────────────────────────────────────────────────

    #[test]
    fn traverse_follows_outbound_edges() {
        let store = store();
        let gid = public_graph(&store);
        let n1 = store
            .upsert_node(
                &gid,
                NodeInput {
                    node_id: None,
                    node_type: "Research".into(),
                    label: "root".into(),
                    content: serde_json::Value::Null,
                    tags: vec![],
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                    table_ref: None,
                },
            )
            .unwrap();
        let n2 = store
            .upsert_node(
                &gid,
                NodeInput {
                    node_id: None,
                    node_type: "Goal".into(),
                    label: "child".into(),
                    content: serde_json::Value::Null,
                    tags: vec![],
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                    table_ref: None,
                },
            )
            .unwrap();
        store
            .upsert_edge(
                &gid,
                EdgeInput {
                    edge_id: None,
                    from_node_id: n1.clone(),
                    to_node_id: n2.clone(),
                    edge_type: "SUPPORTS".into(),
                    label: None,
                    content: serde_json::Value::Null,
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                },
            )
            .unwrap();

        let result = store
            .traverse(
                &gid,
                &TraversalQuery {
                    start_node_id: n1,
                    direction: TraversalDirection::Outbound,
                    max_depth: 2,
                    edge_types: None,
                },
                &alice(),
            )
            .unwrap();

        let node_ids: Vec<_> = result.nodes.iter().map(|n| &n.node_id).collect();
        assert!(node_ids.contains(&&n2));
        assert_eq!(result.edges.len(), 1);
    }

    // ── Traversal (extended) ──────────────────────────────────────────────────

    /// Build a linear chain: n1 → n2 → n3 → n4 in the given graph.
    /// Returns (n1, n2, n3, n4) IDs.
    fn make_chain(
        store: &SqliteGraphStore,
        gid: &str,
        vis: &str,
    ) -> (String, String, String, String) {
        let mk = |label: &str| NodeInput {
            node_id: None,
            node_type: "Research".into(),
            label: label.into(),
            content: serde_json::Value::Null,
            tags: vec![],
            visibility: vec![vis.into()],
            creator: "alice".into(),
            table_ref: None,
        };
        let n1 = store.upsert_node(gid, mk("n1")).unwrap();
        let n2 = store.upsert_node(gid, mk("n2")).unwrap();
        let n3 = store.upsert_node(gid, mk("n3")).unwrap();
        let n4 = store.upsert_node(gid, mk("n4")).unwrap();
        let mk_edge = |from: &str, to: &str, etype: &str| EdgeInput {
            edge_id: None,
            from_node_id: from.into(),
            to_node_id: to.into(),
            edge_type: etype.into(),
            label: None,
            content: serde_json::Value::Null,
            visibility: vec![vis.into()],
            creator: "alice".into(),
        };
        store
            .upsert_edge(gid, mk_edge(&n1, &n2, "SUPPORTS"))
            .unwrap();
        store
            .upsert_edge(gid, mk_edge(&n2, &n3, "SUPPORTS"))
            .unwrap();
        store
            .upsert_edge(gid, mk_edge(&n3, &n4, "SUPPORTS"))
            .unwrap();
        (n1, n2, n3, n4)
    }

    #[test]
    fn traverse_inbound_edges() {
        let store = store();
        let gid = public_graph(&store);
        let (n1, _n2, _n3, n4) = make_chain(&store, &gid, "public");

        // From n4 inbound: should reach n1 (depth 3)
        let result = store
            .traverse(
                &gid,
                &TraversalQuery {
                    start_node_id: n4.clone(),
                    direction: TraversalDirection::Inbound,
                    max_depth: 5,
                    edge_types: None,
                },
                &alice(),
            )
            .unwrap();

        let node_ids: Vec<&String> = result.nodes.iter().map(|n| &n.node_id).collect();
        assert!(
            node_ids.contains(&&n1),
            "inbound traversal from n4 should reach n1"
        );
        assert_eq!(result.edges.len(), 3);
    }

    #[test]
    fn traverse_both_directions() {
        let store = store();
        let gid = public_graph(&store);
        let (n1, n2, n3, n4) = make_chain(&store, &gid, "public");

        // From n2 both: should reach n1 (inbound) and n3, n4 (outbound)
        let result = store
            .traverse(
                &gid,
                &TraversalQuery {
                    start_node_id: n2.clone(),
                    direction: TraversalDirection::Both,
                    max_depth: 5,
                    edge_types: None,
                },
                &alice(),
            )
            .unwrap();

        let node_ids: Vec<&String> = result.nodes.iter().map(|n| &n.node_id).collect();
        assert!(
            node_ids.contains(&&n1),
            "both traversal should reach n1 inbound"
        );
        assert!(
            node_ids.contains(&&n3),
            "both traversal should reach n3 outbound"
        );
        assert!(
            node_ids.contains(&&n4),
            "both traversal should reach n4 outbound"
        );
    }

    #[test]
    fn traverse_respects_max_depth() {
        let store = store();
        let gid = public_graph(&store);
        let (n1, n2, _n3, n4) = make_chain(&store, &gid, "public");

        // max_depth=1 from n1 should only reach n2, not n4
        let result = store
            .traverse(
                &gid,
                &TraversalQuery {
                    start_node_id: n1.clone(),
                    direction: TraversalDirection::Outbound,
                    max_depth: 1,
                    edge_types: None,
                },
                &alice(),
            )
            .unwrap();

        let node_ids: Vec<&String> = result.nodes.iter().map(|n| &n.node_id).collect();
        assert!(node_ids.contains(&&n2), "should reach n2 at depth 1");
        assert!(
            !node_ids.contains(&&n4),
            "should NOT reach n4 at depth 1 cap"
        );
    }

    #[test]
    fn traverse_filters_by_edge_type() {
        let store = store();
        // Use a lenient schema so we can use arbitrary edge types without rejection.
        let gid = store
            .create_graph(GraphSpec {
                name: "lenient".into(),
                description: None,
                schema: GraphSchema {
                    node_types: vec![],
                    edge_types: vec![],
                    strict: false,
                },
                default_visibility: "public".into(),
                creator: "alice".into(),
            })
            .unwrap();
        let mk = |label: &str| NodeInput {
            node_id: None,
            node_type: "Research".into(),
            label: label.into(),
            content: serde_json::Value::Null,
            tags: vec![],
            visibility: vec!["public".into()],
            creator: "alice".into(),
            table_ref: None,
        };
        let n1 = store.upsert_node(&gid, mk("src")).unwrap();
        let n2 = store.upsert_node(&gid, mk("via-supports")).unwrap();
        let n3 = store.upsert_node(&gid, mk("via-blocks")).unwrap();
        store
            .upsert_edge(
                &gid,
                EdgeInput {
                    edge_id: None,
                    from_node_id: n1.clone(),
                    to_node_id: n2.clone(),
                    edge_type: "SUPPORTS".into(),
                    label: None,
                    content: serde_json::Value::Null,
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                },
            )
            .unwrap();
        store
            .upsert_edge(
                &gid,
                EdgeInput {
                    edge_id: None,
                    from_node_id: n1.clone(),
                    to_node_id: n3.clone(),
                    edge_type: "BLOCKS".into(),
                    label: None,
                    content: serde_json::Value::Null,
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                },
            )
            .unwrap();

        let result = store
            .traverse(
                &gid,
                &TraversalQuery {
                    start_node_id: n1.clone(),
                    direction: TraversalDirection::Outbound,
                    max_depth: 2,
                    edge_types: Some(vec!["SUPPORTS".into()]),
                },
                &alice(),
            )
            .unwrap();

        let node_ids: Vec<&String> = result.nodes.iter().map(|n| &n.node_id).collect();
        assert!(node_ids.contains(&&n2), "SUPPORTS edge should be traversed");
        assert!(
            !node_ids.contains(&&n3),
            "BLOCKS edge should be filtered out"
        );
    }

    #[test]
    fn traverse_prunes_at_invisible_nodes() {
        let store = store();
        // Private graph: alice-only nodes form a chain n1 → n2(bob-hidden) → n3
        let gid = private_graph(&store);
        let n1 = store
            .upsert_node(
                &gid,
                NodeInput {
                    node_id: None,
                    node_type: "".into(),
                    label: "n1".into(),
                    content: serde_json::Value::Null,
                    tags: vec![],
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                    table_ref: None,
                },
            )
            .unwrap();
        let n2 = store
            .upsert_node(
                &gid,
                NodeInput {
                    node_id: None,
                    node_type: "".into(),
                    label: "n2-alice-only".into(),
                    content: serde_json::Value::Null,
                    tags: vec![],
                    visibility: vec!["identity:alice".into()],
                    creator: "alice".into(),
                    table_ref: None,
                },
            )
            .unwrap();
        let n3 = store
            .upsert_node(
                &gid,
                NodeInput {
                    node_id: None,
                    node_type: "".into(),
                    label: "n3".into(),
                    content: serde_json::Value::Null,
                    tags: vec![],
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                    table_ref: None,
                },
            )
            .unwrap();
        store
            .upsert_edge(
                &gid,
                EdgeInput {
                    edge_id: None,
                    from_node_id: n1.clone(),
                    to_node_id: n2.clone(),
                    edge_type: "".into(),
                    label: None,
                    content: serde_json::Value::Null,
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                },
            )
            .unwrap();
        store
            .upsert_edge(
                &gid,
                EdgeInput {
                    edge_id: None,
                    from_node_id: n2.clone(),
                    to_node_id: n3.clone(),
                    edge_type: "".into(),
                    label: None,
                    content: serde_json::Value::Null,
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                },
            )
            .unwrap();

        // Alice sees everything
        let alice_result = store
            .traverse(
                &gid,
                &TraversalQuery {
                    start_node_id: n1.clone(),
                    direction: TraversalDirection::Outbound,
                    max_depth: 5,
                    edge_types: None,
                },
                &alice(),
            )
            .unwrap();
        let alice_ids: Vec<&String> = alice_result.nodes.iter().map(|n| &n.node_id).collect();
        assert!(alice_ids.contains(&&n3), "alice should reach n3 through n2");

        // Bob can't see n2, so traversal is pruned — n3 is unreachable
        let bob_result = store
            .traverse(
                &gid,
                &TraversalQuery {
                    start_node_id: n1.clone(),
                    direction: TraversalDirection::Outbound,
                    max_depth: 5,
                    edge_types: None,
                },
                &bob(),
            )
            .unwrap();
        let bob_ids: Vec<&String> = bob_result.nodes.iter().map(|n| &n.node_id).collect();
        assert!(!bob_ids.contains(&&n2), "bob should not see n2");
        assert!(
            !bob_ids.contains(&&n3),
            "bob should not reach n3 (path pruned at n2)"
        );
    }

    // ── Full-text search ──────────────────────────────────────────────────────

    #[test]
    fn fts_search_finds_matching_nodes() {
        let store = store();
        let gid = public_graph(&store);
        let n1 = store
            .upsert_node(
                &gid,
                NodeInput {
                    node_id: None,
                    node_type: "Research".into(),
                    label: "quantum entanglement notes".into(),
                    content: serde_json::json!({ "text": "spooky action at a distance" }),
                    tags: vec![],
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                    table_ref: None,
                },
            )
            .unwrap();
        let _n2 = store
            .upsert_node(
                &gid,
                NodeInput {
                    node_id: None,
                    node_type: "Goal".into(),
                    label: "improve test coverage".into(),
                    content: serde_json::json!({ "text": "increase branch coverage to 90%" }),
                    tags: vec![],
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                    table_ref: None,
                },
            )
            .unwrap();

        let results = store.search_nodes(&gid, "quantum", &alice()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].node_id, n1);
    }

    #[test]
    fn fts_search_respects_visibility() {
        let store = store();
        let gid = private_graph(&store);
        let _n1 = store
            .upsert_node(
                &gid,
                NodeInput {
                    node_id: None,
                    node_type: "".into(),
                    label: "secret finding".into(),
                    content: serde_json::json!({ "text": "classified information here" }),
                    tags: vec![],
                    visibility: vec!["identity:alice".into()],
                    creator: "alice".into(),
                    table_ref: None,
                },
            )
            .unwrap();

        let alice_results = store.search_nodes(&gid, "classified", &alice()).unwrap();
        assert_eq!(alice_results.len(), 1, "alice should find her secret node");

        let bob_results = store.search_nodes(&gid, "classified", &bob()).unwrap();
        assert_eq!(
            bob_results.len(),
            0,
            "bob should not find alice's secret node"
        );
    }

    // ── Schema update ─────────────────────────────────────────────────────────

    #[test]
    fn schema_update_can_add_types() {
        let store = store();
        let gid = public_graph(&store);
        let mut meta = store.get_graph(&gid).unwrap().unwrap();
        meta.schema.node_types.push(NodeTypeSpec {
            name: "Decision".into(),
            description: None,
        });
        store.update_schema(&gid, meta.schema).unwrap();
        let updated = store.get_graph(&gid).unwrap().unwrap();
        assert_eq!(updated.schema.node_types.len(), 3);
    }

    #[test]
    fn schema_update_cannot_remove_used_type() {
        let store = store();
        let gid = public_graph(&store);
        store
            .upsert_node(
                &gid,
                NodeInput {
                    node_id: None,
                    node_type: "Research".into(),
                    label: "test".into(),
                    content: serde_json::Value::Null,
                    tags: vec![],
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                    table_ref: None,
                },
            )
            .unwrap();
        // Try to update schema with Research type removed.
        let schema = GraphSchema {
            node_types: vec![NodeTypeSpec {
                name: "Goal".into(),
                description: None,
            }],
            edge_types: vec![],
            strict: true,
        };
        let result = store.update_schema(&gid, schema);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("cannot remove node type"));
    }

    // ── Table adapter ─────────────────────────────────────────────────────────

    fn sample_columns() -> Vec<ColumnSpec> {
        vec![
            ColumnSpec {
                name: "title".into(),
                col_type: "text".into(),
                description: None,
                required: true,
            },
            ColumnSpec {
                name: "score".into(),
                col_type: "integer".into(),
                description: None,
                required: false,
            },
        ]
    }

    fn make_table(store: &SqliteGraphStore) -> String {
        store
            .create_table(TableSpec {
                name: "findings".into(),
                description: Some("research findings".into()),
                columns: sample_columns(),
                graph_id: None,
                creator: "alice".into(),
            })
            .expect("create table")
    }

    #[test]
    fn create_and_get_table() {
        let store = store();
        let tid = make_table(&store);
        let meta = store.get_table(&tid).unwrap().unwrap();
        assert_eq!(meta.name, "findings");
        assert_eq!(meta.columns.len(), 2);
        assert_eq!(meta.creator, "alice");
    }

    #[test]
    fn list_tables_by_graph_id() {
        let store = store();
        let gid = public_graph(&store);
        store
            .create_table(TableSpec {
                name: "linked-table".into(),
                description: None,
                columns: vec![],
                graph_id: Some(gid.clone()),
                creator: "alice".into(),
            })
            .unwrap();
        store
            .create_table(TableSpec {
                name: "standalone".into(),
                description: None,
                columns: vec![],
                graph_id: None,
                creator: "alice".into(),
            })
            .unwrap();

        let linked = store.list_tables(Some(&gid)).unwrap();
        assert_eq!(linked.len(), 1);
        assert_eq!(linked[0].name, "linked-table");

        let all = store.list_tables(None).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn insert_and_get_row() {
        let store = store();
        let tid = make_table(&store);
        let rid = store
            .insert_row(
                &tid,
                RowInput {
                    row_id: None,
                    data: serde_json::json!({ "title": "First Finding", "score": 9 }),
                    creator: "alice".into(),
                },
            )
            .unwrap();

        let row = store.get_row(&tid, &rid).unwrap().unwrap();
        assert_eq!(row.data["title"], "First Finding");
        assert_eq!(row.data["score"], 9);
    }

    #[test]
    fn update_row_shallow_merge() {
        let store = store();
        let tid = make_table(&store);
        let rid = store
            .insert_row(
                &tid,
                RowInput {
                    row_id: None,
                    data: serde_json::json!({ "title": "Old", "score": 1 }),
                    creator: "alice".into(),
                },
            )
            .unwrap();

        store
            .update_row(&tid, &rid, serde_json::json!({ "title": "New" }))
            .unwrap();
        let row = store.get_row(&tid, &rid).unwrap().unwrap();
        assert_eq!(row.data["title"], "New");
        assert_eq!(row.data["score"], 1, "unchanged field preserved");
    }

    #[test]
    fn delete_row_hides_it() {
        let store = store();
        let tid = make_table(&store);
        let rid = store
            .insert_row(
                &tid,
                RowInput {
                    row_id: None,
                    data: serde_json::json!({ "title": "Gone" }),
                    creator: "alice".into(),
                },
            )
            .unwrap();
        store.delete_row(&tid, &rid).unwrap();
        assert!(store.get_row(&tid, &rid).unwrap().is_none());
    }

    #[test]
    fn query_rows_with_equality_filter() {
        let store = store();
        let tid = make_table(&store);
        store
            .insert_row(
                &tid,
                RowInput {
                    row_id: None,
                    creator: "alice".into(),
                    data: serde_json::json!({ "title": "Alpha", "score": 10 }),
                },
            )
            .unwrap();
        store
            .insert_row(
                &tid,
                RowInput {
                    row_id: None,
                    creator: "alice".into(),
                    data: serde_json::json!({ "title": "Beta", "score": 5 }),
                },
            )
            .unwrap();
        store
            .insert_row(
                &tid,
                RowInput {
                    row_id: None,
                    creator: "alice".into(),
                    data: serde_json::json!({ "title": "Gamma", "score": 10 }),
                },
            )
            .unwrap();

        let results = store
            .query_rows(
                &tid,
                &RowQuery {
                    filter: Some(serde_json::json!({ "score": 10 })),
                    limit: None,
                    offset: None,
                },
            )
            .unwrap();
        assert_eq!(results.len(), 2);
        let titles: Vec<&str> = results
            .iter()
            .map(|r| r.data["title"].as_str().unwrap())
            .collect();
        assert!(titles.contains(&"Alpha"));
        assert!(titles.contains(&"Gamma"));
    }

    #[test]
    fn query_rows_limit_and_offset() {
        let store = store();
        let tid = make_table(&store);
        for i in 0..5u32 {
            store
                .insert_row(
                    &tid,
                    RowInput {
                        row_id: None,
                        creator: "alice".into(),
                        data: serde_json::json!({ "idx": i }),
                    },
                )
                .unwrap();
        }

        let page1 = store
            .query_rows(
                &tid,
                &RowQuery {
                    filter: None,
                    limit: Some(2),
                    offset: Some(0),
                },
            )
            .unwrap();
        assert_eq!(page1.len(), 2);

        let page2 = store
            .query_rows(
                &tid,
                &RowQuery {
                    filter: None,
                    limit: Some(2),
                    offset: Some(2),
                },
            )
            .unwrap();
        assert_eq!(page2.len(), 2);

        // No overlap between pages
        let p1_ids: Vec<&str> = page1.iter().map(|r| r.row_id.as_str()).collect();
        let p2_ids: Vec<&str> = page2.iter().map(|r| r.row_id.as_str()).collect();
        assert!(p1_ids.iter().all(|id| !p2_ids.contains(id)));
    }

    #[test]
    fn drop_table_removes_rows() {
        let store = store();
        let tid = make_table(&store);
        store
            .insert_row(
                &tid,
                RowInput {
                    row_id: None,
                    creator: "alice".into(),
                    data: serde_json::json!({ "title": "Doomed" }),
                },
            )
            .unwrap();
        store.drop_table(&tid).unwrap();
        assert!(store.get_table(&tid).unwrap().is_none());
    }

    #[test]
    fn table_ref_on_node_round_trips() {
        let store = store();
        let gid = public_graph(&store);
        let tid = make_table(&store);

        let nid = store
            .upsert_node(
                &gid,
                NodeInput {
                    node_id: None,
                    node_type: "Research".into(),
                    label: "dataset anchor".into(),
                    content: serde_json::Value::Null,
                    tags: vec![],
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                    table_ref: Some(tid.clone()),
                },
            )
            .unwrap();

        let node = store.get_node(&gid, &nid, &alice()).unwrap().unwrap();
        assert_eq!(node.table_ref.as_deref(), Some(tid.as_str()));
    }

    // ── Export tests ──────────────────────────────────────────────────────────

    #[test]
    fn export_full_graph_returns_all_visible() {
        use crate::graph::{EdgeFilter, NodeFilter};

        let store = store();
        let gid = public_graph(&store);
        let mk_node = |label: &str| NodeInput {
            node_id: None,
            node_type: "Research".into(),
            label: label.into(),
            content: serde_json::Value::Null,
            tags: vec![],
            visibility: vec!["public".into()],
            creator: "alice".into(),
            table_ref: None,
        };
        let n1 = store.upsert_node(&gid, mk_node("A")).unwrap();
        let n2 = store.upsert_node(&gid, mk_node("B")).unwrap();
        let n3 = store.upsert_node(&gid, mk_node("C")).unwrap();
        store
            .upsert_edge(
                &gid,
                EdgeInput {
                    edge_id: None,
                    from_node_id: n1.clone(),
                    to_node_id: n2.clone(),
                    edge_type: "SUPPORTS".into(),
                    label: None,
                    content: serde_json::Value::Null,
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                },
            )
            .unwrap();
        store
            .upsert_edge(
                &gid,
                EdgeInput {
                    edge_id: None,
                    from_node_id: n2.clone(),
                    to_node_id: n3.clone(),
                    edge_type: "SUPPORTS".into(),
                    label: None,
                    content: serde_json::Value::Null,
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                },
            )
            .unwrap();

        let nodes = store
            .list_nodes(&gid, &NodeFilter::default(), &alice())
            .unwrap();
        let edges = store
            .list_edges(&gid, &EdgeFilter::default(), &alice())
            .unwrap();
        assert_eq!(nodes.len(), 3);
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn export_rooted_subgraph_via_traversal() {
        let store = store();
        let gid = store
            .create_graph(GraphSpec {
                name: "export-test".into(),
                description: None,
                schema: GraphSchema {
                    node_types: vec![],
                    edge_types: vec![],
                    strict: false,
                },
                default_visibility: "public".into(),
                creator: "alice".into(),
            })
            .unwrap();
        let mk = |label: &str| NodeInput {
            node_id: None,
            node_type: "X".into(),
            label: label.into(),
            content: serde_json::Value::Null,
            tags: vec![],
            visibility: vec!["public".into()],
            creator: "alice".into(),
            table_ref: None,
        };
        let root = store.upsert_node(&gid, mk("root")).unwrap();
        let child = store.upsert_node(&gid, mk("child")).unwrap();
        let orphan = store.upsert_node(&gid, mk("orphan")).unwrap();
        store
            .upsert_edge(
                &gid,
                EdgeInput {
                    edge_id: None,
                    from_node_id: root.clone(),
                    to_node_id: child.clone(),
                    edge_type: "HAS".into(),
                    label: None,
                    content: serde_json::Value::Null,
                    visibility: vec!["public".into()],
                    creator: "alice".into(),
                },
            )
            .unwrap();

        // Traversal from root (both directions, depth 10) should include root + child but not orphan.
        let result = store
            .traverse(
                &gid,
                &crate::graph::TraversalQuery {
                    start_node_id: root.clone(),
                    direction: crate::graph::TraversalDirection::Both,
                    max_depth: 10,
                    edge_types: None,
                },
                &alice(),
            )
            .unwrap();

        let node_ids: Vec<_> = result.nodes.iter().map(|n| n.node_id.as_str()).collect();
        assert!(node_ids.contains(&root.as_str()));
        assert!(node_ids.contains(&child.as_str()));
        assert!(!node_ids.contains(&orphan.as_str()));
        // Both-direction traversal may surface the connecting edge from each direction;
        // assert at least one edge connecting root and child is present.
        assert!(!result.edges.is_empty());
        assert!(result.edges.iter().any(|e| {
            (e.from_node_id == root && e.to_node_id == child)
                || (e.from_node_id == child && e.to_node_id == root)
        }));
    }
}
