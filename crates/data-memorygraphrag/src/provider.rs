use anyhow::{Context, Result};
use async_trait::async_trait;
use data_memorygraphrag::cypher;
use data_memorygraphrag::projection;
use data_memorygraphrag::{
    LifeGraphToolRequest, LifeObserveInput, MemoryGraphRagRunner, PolicyFilter, RetrievalQuery,
    RunnerConfig,
};
use datasource::controller::{DatasourceProvider, DatasourceTask, ProviderOutput};
use neo4rs::{
    BoltList, BoltMap, BoltNode, BoltRelation, BoltType, BoltUnboundedRelation, ConfigBuilder,
    Graph, Row, query,
};
use serde_json::{Value, json};
use tracing::{info, warn};

struct MemgraphConfig {
    uri: String,
    user: String,
    password: String,
}

impl MemgraphConfig {
    fn from_env() -> Self {
        Self {
            uri: std::env::var("PHILOTIC_MEMGRAPH_URI")
                .unwrap_or_else(|_| "127.0.0.1:7687".to_string()),
            user: std::env::var("PHILOTIC_MEMGRAPH_USER")
                .or_else(|_| std::env::var("MEMGRAPH_USER"))
                .unwrap_or_default(),
            password: std::env::var("PHILOTIC_MEMGRAPH_PASSWORD")
                .or_else(|_| std::env::var("MEMGRAPH_PASSWORD"))
                .unwrap_or_default(),
        }
    }
}

pub struct LifeGraphProvider {
    config: MemgraphConfig,
    runner: MemoryGraphRagRunner,
}

impl LifeGraphProvider {
    pub fn from_env() -> Self {
        let datasource_id = std::env::var("PHILOTIC_LIFE_GRAPH_DATASOURCE_ID")
            .unwrap_or_else(|_| "life-graph".to_string());
        Self {
            config: MemgraphConfig::from_env(),
            runner: MemoryGraphRagRunner::new(RunnerConfig {
                datasource_id,
                default_embedding_model: "text-embedding-3-small".to_string(),
            }),
        }
    }

    async fn connect(&self) -> Result<Graph> {
        let mut builder = ConfigBuilder::default()
            .uri(self.config.uri.as_str())
            .user(self.config.user.as_str())
            .password(self.config.password.as_str());

        if let Ok(db) = std::env::var("PHILOTIC_MEMGRAPH_DB") {
            if !db.is_empty() {
                builder = builder.db(db.as_str());
            }
        }

        Ok(Graph::connect(builder.build()?)?)
    }

    async fn execute_cypher(&self, cypher: &str) -> Result<Value> {
        let graph = self.connect().await?;
        let mut rows = graph.execute(query(cypher)).await?;
        let mut output = Vec::new();
        while let Some(row) = rows.next().await? {
            output.push(row_to_json(&row)?);
        }
        Ok(json!({ "rows": output }))
    }
}

#[async_trait]
impl DatasourceProvider for LifeGraphProvider {
    fn id(&self) -> &str {
        "life-graph-memorygraphrag"
    }

    fn supports(&self, task: &DatasourceTask) -> bool {
        task.kind.as_str().starts_with("life.")
    }

    async fn invoke(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        match task.kind.as_str() {
            "life.observe" => self.handle_observe(task).await,
            "life.recall" => self.handle_recall(task).await,
            other => {
                warn!(tool = other, "life.* tool not yet implemented in runner");
                Ok(ProviderOutput::ResultSet(json!({
                    "status": "not_yet_implemented_in_runner",
                    "tool": other,
                })))
            }
        }
    }
}

impl LifeGraphProvider {
    async fn handle_observe(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        let input: LifeObserveInput = serde_json::from_value(task.parameters.clone())
            .context("failed to parse life.observe parameters as LifeObserveInput")?;

        let plan = self
            .runner
            .plan(LifeGraphToolRequest::LifeObserve(input.clone()))
            .map_err(|e| anyhow::anyhow!("life.observe plan validation failed: {e}"))?;

        if !plan.allowed() {
            return Ok(ProviderOutput::ResultSet(json!({
                "status": "blocked",
                "reasons": plan.blocked_reasons,
            })));
        }

        let now = chrono::Utc::now().to_rfc3339();
        let compiled = cypher::compile_observe(&input, &now)
            .map_err(|e| anyhow::anyhow!("Cypher compilation failed: {e}"))?;

        let graph = self.connect().await?;

        let q = query(&compiled.query)
            .param("id", compiled.node_id.as_str())
            .param("created_at", compiled.created_at.as_str())
            .param("source_membrane", compiled.source_membrane.as_str())
            .param("provenance", compiled.provenance.as_str())
            .param("confidence", compiled.confidence)
            .param("validation_state", compiled.validation_state.as_str())
            .param("observed_at", compiled.observed_at.as_str())
            .param("claim_summary", compiled.claim_summary.as_str())
            .param("observation_id", compiled.observation_id.as_str())
            .param("packet_id", compiled.packet_id.as_str());

        let mut rows = graph.execute(q).await?;
        let first_row = rows.next().await?;

        let node_id = first_row
            .as_ref()
            .and_then(|r| r.get::<String>("id").ok())
            .unwrap_or_else(|| compiled.node_id.clone());

        info!(
            node_id = %node_id,
            label = %compiled.label,
            observation_id = %compiled.observation_id,
            packet_id = %compiled.packet_id,
            "life.observe: proposed evidence node written to Memgraph"
        );

        Ok(ProviderOutput::ResultSet(json!({
            "status": "proposed",
            "node_id": node_id,
            "label": compiled.label,
            "observation_id": compiled.observation_id,
            "packet_id": compiled.packet_id,
            "validation_state": compiled.validation_state,
        })))
    }

    async fn handle_recall(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        let query_val: RetrievalQuery = serde_json::from_value(task.parameters.clone())
            .context("failed to parse life.recall parameters as RetrievalQuery")?;

        self.runner
            .plan(LifeGraphToolRequest::LifeRecall(query_val.clone()))
            .map_err(|e| anyhow::anyhow!("life.recall plan validation failed: {e}"))?;

        // Embedding vector must be passed inline in the task parameters.
        let embedding: Vec<f32> = task
            .parameters
            .get("embedding")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_f64().map(|f| f as f32))
                    .collect()
            })
            .unwrap_or_default();

        if embedding.is_empty() {
            return Ok(ProviderOutput::ResultSet(json!({
                "status": "missing_embedding",
                "detail": "life.recall requires an inline 'embedding' array in parameters",
            })));
        }

        let top_k = query_val.max_context_packets * 3;
        let min_similarity = 0.3_f32;
        let now = chrono::Utc::now();
        let now_iso = now.to_rfc3339();

        let mut all_hits = Vec::new();

        for pivot in &query_val.semantic_pivots {
            for label in projection::labels_for_space(&pivot.space) {
                let index = projection::index_name(&pivot.space, label);
                let cypher =
                    projection::semantic_expand_cypher(&index, top_k, &embedding, min_similarity);

                let result = self.execute_cypher(&cypher).await?;
                let hits = projection::parse_vector_search_rows(&result);
                all_hits.extend(hits);
            }
        }

        let filters = &query_val.policy_filters;
        let (surviving, _drop_log) = projection::apply_policy_filters(all_hits, filters);

        let weights = &query_val.ranking_weights;
        let mut scored: Vec<(projection::VectorHit, f32, Vec<PolicyFilter>)> = surviving
            .into_iter()
            .map(|hit| {
                let age_secs = compute_age_secs(hit.prop_str("observed_at"), &now);
                let score = projection::ranking_score(&hit, weights, age_secs);
                (hit, score, Vec::new())
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let context_id = format!("ctx:{}", query_val.query_id);
        let token_budget = query_val.max_context_packets * 200;
        let packet = projection::project_context_packet(
            &context_id,
            &query_val.query_id,
            query_val.strategy.clone(),
            scored,
            Vec::new(),
            token_budget,
            &now_iso,
        );

        info!(
            query_id = %query_val.query_id,
            result_count = packet.ranked_packets.len(),
            "life.recall: context packet projected"
        );

        let packet_json =
            serde_json::to_value(&packet).context("failed to serialize RetrievalContextPacket")?;

        Ok(ProviderOutput::ResultSet(json!({
            "status": "ok",
            "context_packet": packet_json,
        })))
    }
}

/// Compute seconds elapsed since `observed_at` ISO 8601 string.
/// Returns 0 on parse failure (treat unknown age as fresh).
fn compute_age_secs(observed_at: Option<&str>, now: &chrono::DateTime<chrono::Utc>) -> u64 {
    observed_at
        .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
        .map(|dt| {
            let elapsed = *now - dt.with_timezone(&chrono::Utc);
            elapsed.num_seconds().max(0) as u64
        })
        .unwrap_or(0)
}

// ── Bolt → JSON conversion (mirrors graph-datasource/memgraph_provider.rs) ────

fn row_to_json(row: &Row) -> Result<Value> {
    let mut object = serde_json::Map::new();
    for key in row.keys() {
        let key = key.value.as_str();
        let value: BoltType = row.get(key)?;
        object.insert(key.to_string(), bolt_value_to_json(value));
    }
    Ok(Value::Object(object))
}

fn bolt_value_to_json(value: BoltType) -> Value {
    match value {
        BoltType::String(v) => json!(v.value),
        BoltType::Boolean(v) => json!(v.value),
        BoltType::Integer(v) => json!(v.value),
        BoltType::Float(v) => json!(v.value),
        BoltType::Null(_) => Value::Null,
        BoltType::List(v) => bolt_list_to_json(v),
        BoltType::Map(v) => bolt_map_to_json(v),
        BoltType::Node(v) => bolt_node_to_json(v),
        BoltType::Relation(v) => bolt_relation_to_json(v),
        BoltType::UnboundedRelation(v) => bolt_unbounded_relation_to_json(v),
        BoltType::Bytes(v) => json!(v.value),
        other => json!({ "kind": "unsupported_bolt_value", "debug": format!("{other:?}") }),
    }
}

fn bolt_list_to_json(v: BoltList) -> Value {
    Value::Array(v.into_iter().map(bolt_value_to_json).collect())
}

fn bolt_map_to_json(v: BoltMap) -> Value {
    Value::Object(
        v.value
            .into_iter()
            .map(|(k, val)| (k.value, bolt_value_to_json(val)))
            .collect(),
    )
}

fn bolt_node_to_json(v: BoltNode) -> Value {
    json!({
        "kind": "node",
        "id": v.id.value,
        "labels": bolt_list_to_json(v.labels),
        "properties": bolt_map_to_json(v.properties),
    })
}

fn bolt_relation_to_json(v: BoltRelation) -> Value {
    json!({
        "kind": "relationship",
        "id": v.id.value,
        "source": v.start_node_id.value,
        "target": v.end_node_id.value,
        "label": v.typ.value,
        "properties": bolt_map_to_json(v.properties),
    })
}

fn bolt_unbounded_relation_to_json(v: BoltUnboundedRelation) -> Value {
    json!({
        "kind": "relationship",
        "id": v.id.value,
        "label": v.typ.value,
        "properties": bolt_map_to_json(v.properties),
    })
}
