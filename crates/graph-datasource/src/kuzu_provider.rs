use anyhow::{Result, bail};
use async_trait::async_trait;
use datasource::controller::{DatasourceProvider, DatasourceTask, ProviderOutput, TaskKind};
use kuzu::{Connection, Database, SystemConfig, Value as KuzuValue};
use serde_json::{Value as JsonValue, json};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

pub struct KuzuCypherProvider {
    pub base_dir: PathBuf,
}

impl KuzuCypherProvider {
    pub fn new<P: AsRef<Path>>(base_dir: P) -> Self {
        Self {
            base_dir: base_dir.as_ref().to_path_buf(),
        }
    }

    fn get_db_path(&self, graph_id: &str) -> Result<PathBuf> {
        if graph_id.is_empty() || graph_id.contains("..") || graph_id.contains('/') {
            bail!("invalid graph_id: {}", graph_id);
        }
        Ok(self.base_dir.join(format!("{}.kuzu", graph_id)))
    }

    fn open_partition(&self, path: &Path) -> Result<Database> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Database::new(path, SystemConfig::default())?)
    }

    fn execute_cypher(&self, db_path: &Path, query: &str) -> Result<JsonValue> {
        let db = self.open_partition(db_path)?;
        let conn = Connection::new(&db)?;
        let mut result = conn.query(query)?;
        let columns = result.get_column_names();
        let mut rows = Vec::new();

        for row in &mut result {
            let mut object = serde_json::Map::new();
            for (index, value) in row.into_iter().enumerate() {
                let key = columns
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| format!("column_{index}"));
                object.insert(key, kuzu_value_to_json(value));
            }
            rows.push(JsonValue::Object(object));
        }

        Ok(json!({
            "columns": columns,
            "rows": rows,
        }))
    }
}

#[async_trait]
impl DatasourceProvider for KuzuCypherProvider {
    fn id(&self) -> &str {
        "kuzu-cypher"
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
                info!("Creating Kuzu partition [{}] at {:?}", graph_id, db_path);
                self.open_partition(&db_path)?;
                Ok(ProviderOutput::PartitionCreated {
                    graph_id: graph_id.to_string(),
                })
            }
            TaskKind::DropPartition => {
                info!("Dropping Kuzu partition [{}]", graph_id);
                if db_path.is_dir() {
                    std::fs::remove_dir_all(&db_path)?;
                } else if db_path.exists() {
                    std::fs::remove_file(&db_path)?;
                }
                Ok(ProviderOutput::Acknowledge)
            }
            TaskKind::Custom(s) if s == "graph.list" => {
                info!("Listing Kuzu graph partitions in {:?}", self.base_dir);
                let mut graphs = Vec::new();
                if self.base_dir.exists() {
                    for entry in std::fs::read_dir(&self.base_dir)? {
                        let entry = entry?;
                        let path = entry.path();
                        if path.extension().and_then(|s| s.to_str()) == Some("kuzu") {
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
                let result = self.execute_cypher(&db_path, query)?;
                Ok(ProviderOutput::ResultSet(result))
            }
            _ => bail!("unsupported task kind for kuzu-cypher"),
        }
    }
}

fn kuzu_value_to_json(value: KuzuValue) -> JsonValue {
    match value {
        KuzuValue::Null(_) => JsonValue::Null,
        KuzuValue::Bool(value) => json!(value),
        KuzuValue::Int64(value) => json!(value),
        KuzuValue::Int32(value) => json!(value),
        KuzuValue::Int16(value) => json!(value),
        KuzuValue::Int8(value) => json!(value),
        KuzuValue::UInt64(value) => json!(value),
        KuzuValue::UInt32(value) => json!(value),
        KuzuValue::UInt16(value) => json!(value),
        KuzuValue::UInt8(value) => json!(value),
        KuzuValue::Double(value) => json!(value),
        KuzuValue::Float(value) => json!(value),
        KuzuValue::String(value) => json!(value),
        KuzuValue::Node(node) => {
            let properties = node
                .get_properties()
                .iter()
                .map(|(key, value)| (key.clone(), kuzu_value_to_json(value.clone())))
                .collect::<serde_json::Map<_, _>>();
            json!({
                "kind": "node",
                "id": node.get_node_id().to_string(),
                "label": node.get_label_name(),
                "properties": properties,
            })
        }
        KuzuValue::Rel(rel) => {
            let properties = rel
                .get_properties()
                .iter()
                .map(|(key, value)| (key.clone(), kuzu_value_to_json(value.clone())))
                .collect::<serde_json::Map<_, _>>();
            json!({
                "kind": "relationship",
                "source": rel.get_src_node().to_string(),
                "target": rel.get_dst_node().to_string(),
                "label": rel.get_label_name(),
                "properties": properties,
            })
        }
        KuzuValue::List(_, values) | KuzuValue::Array(_, values) => {
            JsonValue::Array(values.into_iter().map(kuzu_value_to_json).collect())
        }
        KuzuValue::Struct(values) => JsonValue::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, kuzu_value_to_json(value)))
                .collect(),
        ),
        KuzuValue::Map(_, values) => JsonValue::Array(
            values
                .into_iter()
                .map(|(key, value)| {
                    json!({
                        "key": kuzu_value_to_json(key),
                        "value": kuzu_value_to_json(value),
                    })
                })
                .collect(),
        ),
        other => json!(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use datasource::controller::DatasourceTask;

    async fn query(
        provider: &KuzuCypherProvider,
        graph_id: &str,
        query: &str,
    ) -> Result<JsonValue> {
        let task = DatasourceTask {
            kind: TaskKind::Query,
            provider: Some("kuzu-cypher".to_string()),
            db: None,
            graph_id: Some(graph_id.to_string()),
            query: Some(query.to_string()),
            parameters: json!({}),
            identity: json!({"agent_id": "test"}),
        };
        match provider.invoke(&task).await? {
            ProviderOutput::ResultSet(value) => Ok(value),
            other => bail!("unexpected provider output: {:?}", other),
        }
    }

    #[tokio::test]
    async fn supports_beacon_style_goal_merge_and_relationship_return() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let provider = KuzuCypherProvider::new(temp.path());
        let graph_id = "beacon";

        query(
            &provider,
            graph_id,
            "CREATE NODE TABLE Role(id STRING, name STRING, PRIMARY KEY(id));",
        )
        .await?;
        query(
            &provider,
            graph_id,
            "CREATE NODE TABLE Goal(id STRING, name STRING, description STRING, PRIMARY KEY(id));",
        )
        .await?;
        query(
            &provider,
            graph_id,
            "CREATE REL TABLE HAS_GOAL(FROM Role TO Goal, id STRING);",
        )
        .await?;

        query(
            &provider,
            graph_id,
            "MERGE (r:Role {id: 'human'}) SET r.name = 'Human';",
        )
        .await?;
        query(
            &provider,
            graph_id,
            "MERGE (g:Goal {id: 'enhance_appearance'}) SET g.name = 'Enhance my Appearance', g.description = 'Enhance my appearance as a human goal';",
        )
        .await?;
        let result = query(
            &provider,
            graph_id,
            "MATCH (r:Role {id: 'human'}), (g:Goal {id: 'enhance_appearance'}) MERGE (r)-[rel:HAS_GOAL {id: 'human-enhance_appearance'}]->(g) RETURN r, rel, g;",
        )
        .await?;

        let rows = result
            .get("rows")
            .and_then(|value| value.as_array())
            .expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["r"]["properties"]["id"], "human");
        assert_eq!(rows[0]["rel"]["label"], "HAS_GOAL");
        assert_eq!(rows[0]["g"]["properties"]["id"], "enhance_appearance");
        Ok(())
    }
}
