use crate::transpiler::{TranslatedQuery, transpile_cypher};
use anyhow::{Result, bail};
use async_trait::async_trait;
use datasource::controller::{DatasourceProvider, DatasourceTask, ProviderOutput, TaskKind};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

pub struct SqliteCypherProvider {
    pub base_dir: PathBuf,
}

impl SqliteCypherProvider {
    pub fn new<P: AsRef<Path>>(base_dir: P) -> Self {
        Self {
            base_dir: base_dir.as_ref().to_path_buf(),
        }
    }

    fn get_db_path(&self, graph_id: &str) -> Result<PathBuf> {
        if graph_id.is_empty() || graph_id.contains("..") || graph_id.contains('/') {
            bail!("invalid graph_id: {}", graph_id);
        }
        Ok(self.base_dir.join(format!("{}.db", graph_id)))
    }

    fn init_partition(&self, path: &PathBuf) -> Result<()> {
        let conn = Connection::open(path)?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS ag_node (
                id TEXT PRIMARY KEY,
                label TEXT NOT NULL,
                properties TEXT NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS ag_edge (
                id TEXT PRIMARY KEY,
                source_id TEXT NOT NULL,
                target_id TEXT NOT NULL,
                label TEXT NOT NULL,
                properties TEXT NOT NULL,
                FOREIGN KEY(source_id) REFERENCES ag_node(id) ON DELETE CASCADE,
                FOREIGN KEY(target_id) REFERENCES ag_node(id) ON DELETE CASCADE
            )",
            [],
        )?;
        Ok(())
    }

    fn execute_cypher(&self, conn: &Connection, query: &str, _params: &Value) -> Result<Value> {
        let translated = transpile_cypher(query)?;
        match translated {
            TranslatedQuery::InsertNode {
                id,
                label,
                properties,
            } => {
                let props_json = serde_json::to_string(&properties)?;
                conn.execute(
                    "INSERT OR REPLACE INTO ag_node (id, label, properties) VALUES (?, ?, ?)",
                    params![id, label, props_json],
                )?;
                Ok(json!({"status": "created", "id": id}))
            }
            TranslatedQuery::InsertEdge {
                id,
                source_id,
                target_id,
                label,
                properties,
            } => {
                let props_json = serde_json::to_string(&properties)?;
                conn.execute(
                    "INSERT OR REPLACE INTO ag_edge (id, source_id, target_id, label, properties) VALUES (?, ?, ?, ?, ?)",
                    params![id, source_id, target_id, label, props_json],
                )?;
                Ok(json!({"status": "created", "id": id}))
            }
            TranslatedQuery::DeleteNode { id } => {
                conn.execute("DELETE FROM ag_node WHERE id = ?", params![id])?;
                Ok(json!({"status": "deleted", "id": id}))
            }
            TranslatedQuery::DeleteEdge { id } => {
                conn.execute("DELETE FROM ag_edge WHERE id = ?", params![id])?;
                Ok(json!({"status": "deleted", "id": id}))
            }
            TranslatedQuery::SelectNodes { label, filters: _ } => {
                let mut results = Vec::new();
                if let Some(l) = label {
                    let mut stmt =
                        conn.prepare("SELECT id, label, properties FROM ag_node WHERE label = ?")?;
                    let rows = stmt.query_map(params![l], |row| {
                        let id: String = row.get(0)?;
                        let label: String = row.get(1)?;
                        let props: String = row.get(2)?;
                        let properties: Value = serde_json::from_str(&props).unwrap_or(json!({}));
                        Ok(json!({"id": id, "label": label, "properties": properties}))
                    })?;
                    for row in rows {
                        results.push(row?);
                    }
                } else {
                    let mut stmt = conn.prepare("SELECT id, label, properties FROM ag_node")?;
                    let rows = stmt.query_map([], |row| {
                        let id: String = row.get(0)?;
                        let label: String = row.get(1)?;
                        let props: String = row.get(2)?;
                        let properties: Value = serde_json::from_str(&props).unwrap_or(json!({}));
                        Ok(json!({"id": id, "label": label, "properties": properties}))
                    })?;
                    for row in rows {
                        results.push(row?);
                    }
                }
                Ok(json!(results))
            }
        }
    }
}

#[async_trait]
impl DatasourceProvider for SqliteCypherProvider {
    fn id(&self) -> &str {
        "sqlite-cypher"
    }

    fn supports(&self, task: &DatasourceTask) -> bool {
        matches!(
            task.kind,
            TaskKind::Query
                | TaskKind::CreatePartition
                | TaskKind::DropPartition
                | TaskKind::GrantAccess
        ) || matches!(&task.kind, TaskKind::Custom(s) if s == "graph.list")
    }

    async fn invoke(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        let graph_id = task.graph_id.as_deref().unwrap_or("default");
        let db_path = self.get_db_path(graph_id)?;

        match &task.kind {
            TaskKind::CreatePartition => {
                info!("Creating partition [{}] at {:?}", graph_id, db_path);
                if let Some(parent) = db_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                self.init_partition(&db_path)?;
                Ok(ProviderOutput::PartitionCreated {
                    graph_id: graph_id.to_string(),
                })
            }
            TaskKind::DropPartition => {
                info!("Dropping partition [{}]", graph_id);
                if db_path.exists() {
                    std::fs::remove_file(&db_path)?;
                }
                Ok(ProviderOutput::Acknowledge)
            }
            TaskKind::Custom(s) if s == "graph.list" => {
                info!("Listing graph partitions in {:?}", self.base_dir);
                let mut graphs = Vec::new();
                if self.base_dir.exists() {
                    for entry in std::fs::read_dir(&self.base_dir)? {
                        let entry = entry?;
                        let path = entry.path();
                        if path.extension().and_then(|s| s.to_str()) == Some("db") {
                            if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                                graphs.push(json!({
                                    "graph_id": name,
                                    "role": "owner"
                                }));
                            }
                        }
                    }
                }
                Ok(ProviderOutput::ResultSet(json!(graphs)))
            }
            TaskKind::GrantAccess => {
                warn!("GrantAccess is a stub — no persistent permissions store yet");
                Ok(ProviderOutput::Acknowledge)
            }
            TaskKind::Query => {
                let query = match &task.query {
                    Some(q) => q,
                    None => bail!("Query task requires 'query' field"),
                };

                if !db_path.exists() {
                    if let Some(parent) = db_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    self.init_partition(&db_path)?;
                }

                let conn = Connection::open(&db_path)?;
                let result = self.execute_cypher(&conn, query, &task.parameters)?;
                Ok(ProviderOutput::ResultSet(result))
            }
            _ => bail!("unsupported task kind for sqlite-cypher"),
        }
    }
}
