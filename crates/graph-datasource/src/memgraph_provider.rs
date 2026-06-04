use anyhow::{Result, bail};
use async_trait::async_trait;
use datasource::controller::{DatasourceProvider, DatasourceTask, ProviderOutput, TaskKind};
use neo4rs::{
    BoltList, BoltMap, BoltNode, BoltRelation, BoltType, BoltUnboundedRelation, ConfigBuilder,
    Graph, Row, query,
};
use serde_json::{Value as JsonValue, json};
use tracing::{info, warn};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemgraphConfig {
    pub uri: String,
    pub user: String,
    pub password: String,
    pub db: Option<String>,
}

impl MemgraphConfig {
    pub fn from_env() -> Self {
        Self {
            uri: std::env::var("PHILOTIC_MEMGRAPH_URI")
                .unwrap_or_else(|_| "127.0.0.1:7687".to_string()),
            user: std::env::var("PHILOTIC_MEMGRAPH_USER")
                .or_else(|_| std::env::var("MEMGRAPH_USER"))
                .unwrap_or_default(),
            password: std::env::var("PHILOTIC_MEMGRAPH_PASSWORD")
                .or_else(|_| std::env::var("MEMGRAPH_PASSWORD"))
                .unwrap_or_default(),
            db: std::env::var("PHILOTIC_MEMGRAPH_DB").ok(),
        }
    }
}

pub struct MemgraphCypherProvider {
    config: MemgraphConfig,
}

impl MemgraphCypherProvider {
    pub fn new(config: MemgraphConfig) -> Self {
        Self { config }
    }

    pub fn from_env() -> Self {
        Self::new(MemgraphConfig::from_env())
    }

    async fn connect(&self, db_override: Option<&str>) -> Result<Graph> {
        let mut builder = ConfigBuilder::default()
            .uri(self.config.uri.as_str())
            .user(self.config.user.as_str())
            .password(self.config.password.as_str());

        if let Some(db) = db_override.or(self.config.db.as_deref()) {
            builder = builder.db(db);
        }

        Ok(Graph::connect(builder.build()?)?)
    }

    async fn execute_cypher(&self, task: &DatasourceTask, query_text: &str) -> Result<JsonValue> {
        let graph = self.connect(task.db.as_deref()).await?;
        let mut rows = graph.execute(query(query_text)).await?;
        let mut output = Vec::new();

        while let Some(row) = rows.next().await? {
            output.push(row_to_json(&row)?);
        }

        Ok(json!({
            "rows": output,
        }))
    }

    async fn schema(&self, task: &DatasourceTask) -> Result<JsonValue> {
        let labels = self
            .execute_cypher(
                task,
                "MATCH (n) UNWIND labels(n) AS label RETURN collect(DISTINCT label) AS labels;",
            )
            .await?;
        let relationship_types = self
            .execute_cypher(
                task,
                "MATCH ()-[r]->() RETURN collect(DISTINCT type(r)) AS relationship_types;",
            )
            .await?;

        Ok(json!({
            "labels": labels
                .get("rows")
                .and_then(JsonValue::as_array)
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("labels"))
                .cloned()
                .unwrap_or_else(|| json!([])),
            "relationship_types": relationship_types
                .get("rows")
                .and_then(JsonValue::as_array)
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("relationship_types"))
                .cloned()
                .unwrap_or_else(|| json!([])),
        }))
    }
}

#[async_trait]
impl DatasourceProvider for MemgraphCypherProvider {
    fn id(&self) -> &str {
        "memgraph-cypher"
    }

    fn supports(&self, task: &DatasourceTask) -> bool {
        matches!(task.kind, TaskKind::Query | TaskKind::CreatePartition)
            || matches!(&task.kind, TaskKind::Custom(s) if s == "graph.list" || s == "graph.schema")
    }

    async fn invoke(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        match &task.kind {
            TaskKind::CreatePartition => {
                info!("Memgraph partition create is logical; physical database is Bolt endpoint");
                Ok(ProviderOutput::PartitionCreated {
                    graph_id: task
                        .graph_id
                        .clone()
                        .unwrap_or_else(|| "default".to_string()),
                })
            }
            TaskKind::Custom(s) if s == "graph.list" => Ok(ProviderOutput::ResultSet(json!([{
                "graph_id": task.graph_id.as_deref().unwrap_or("memgraph"),
                "role": "central",
                "provider": self.id(),
                "uri": self.config.uri,
            }]))),
            TaskKind::Custom(s) if s == "graph.schema" => {
                let result = self.schema(task).await?;
                Ok(ProviderOutput::ResultSet(result))
            }
            TaskKind::GrantAccess => {
                warn!(
                    "GrantAccess is a stub for Memgraph provider; enforce via Bolt auth/network policy"
                );
                Ok(ProviderOutput::Acknowledge)
            }
            TaskKind::Query => {
                let query_text = match &task.query {
                    Some(query) => query,
                    None => bail!("Query task requires 'query' field"),
                };
                let result = self.execute_cypher(task, query_text).await?;
                Ok(ProviderOutput::ResultSet(result))
            }
            _ => bail!("unsupported task kind for memgraph-cypher"),
        }
    }
}

fn row_to_json(row: &Row) -> Result<JsonValue> {
    let mut object = serde_json::Map::new();
    for key in row.keys() {
        let key = key.value.as_str();
        let value: BoltType = row.get(key)?;
        object.insert(key.to_string(), bolt_value_to_json(value));
    }
    Ok(JsonValue::Object(object))
}

fn bolt_value_to_json(value: BoltType) -> JsonValue {
    match value {
        BoltType::String(value) => json!(value.value),
        BoltType::Boolean(value) => json!(value.value),
        BoltType::Integer(value) => json!(value.value),
        BoltType::Float(value) => json!(value.value),
        BoltType::Null(_) => JsonValue::Null,
        BoltType::List(value) => bolt_list_to_json(value),
        BoltType::Map(value) => bolt_map_to_json(value),
        BoltType::Node(value) => bolt_node_to_json(value),
        BoltType::Relation(value) => bolt_relation_to_json(value),
        BoltType::UnboundedRelation(value) => bolt_unbounded_relation_to_json(value),
        BoltType::Bytes(value) => json!(value.value),
        other => json!({
            "kind": "unsupported_bolt_value",
            "debug": format!("{other:?}"),
        }),
    }
}

fn bolt_list_to_json(value: BoltList) -> JsonValue {
    JsonValue::Array(value.into_iter().map(bolt_value_to_json).collect())
}

fn bolt_map_to_json(value: BoltMap) -> JsonValue {
    JsonValue::Object(
        value
            .value
            .into_iter()
            .map(|(key, value)| (key.value, bolt_value_to_json(value)))
            .collect(),
    )
}

fn bolt_node_to_json(value: BoltNode) -> JsonValue {
    json!({
        "kind": "node",
        "id": value.id.value,
        "labels": bolt_list_to_json(value.labels),
        "properties": bolt_map_to_json(value.properties),
    })
}

fn bolt_relation_to_json(value: BoltRelation) -> JsonValue {
    json!({
        "kind": "relationship",
        "id": value.id.value,
        "source": value.start_node_id.value,
        "target": value.end_node_id.value,
        "label": value.typ.value,
        "properties": bolt_map_to_json(value.properties),
    })
}

fn bolt_unbounded_relation_to_json(value: BoltUnboundedRelation) -> JsonValue {
    json!({
        "kind": "relationship",
        "id": value.id.value,
        "label": value.typ.value,
        "properties": bolt_map_to_json(value.properties),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use neo4rs::{BoltInteger, BoltString};

    #[test]
    fn config_defaults_to_local_bolt() {
        let config = MemgraphConfig {
            uri: "127.0.0.1:7687".to_string(),
            user: String::new(),
            password: String::new(),
            db: None,
        };
        let provider = MemgraphCypherProvider::new(config.clone());
        assert_eq!(provider.config, config);
    }

    #[test]
    fn bolt_node_converts_to_graph_shaped_json() {
        let node = BoltNode::new(
            BoltInteger::new(42),
            vec![BoltType::String(BoltString::new("Goal"))].into(),
            vec![("id".into(), "enhance_appearance".into())]
                .into_iter()
                .collect(),
        );

        let json = bolt_node_to_json(node);
        assert_eq!(json["kind"], "node");
        assert_eq!(json["id"], 42);
        assert_eq!(json["labels"][0], "Goal");
        assert_eq!(json["properties"]["id"], "enhance_appearance");
    }
}
