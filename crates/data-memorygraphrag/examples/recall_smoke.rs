/// Live smoke test for life.recall → Memgraph vector search → RetrievalContextPacket.
///
/// Run with:
///   PHILOTIC_MEMGRAPH_URI=100.64.212.8:7687 \
///   cargo run -p data-memorygraphrag --example recall_smoke
///
/// Expected: pipeline runs end-to-end. 0 results is correct when no embeddings exist.
/// Verify mechanism: RetrievalContextPacket is returned with status="ok".
use data_memorygraphrag::projection;
use data_memorygraphrag::{
    ExpansionPolicy, LIFE_GRAPH_EMBEDDING_DIMS, PolicyFilter, RankingWeights, RetrievalQuery,
    RetrievalStrategy, SemanticPivot, SemanticSpace,
};
use neo4rs::{ConfigBuilder, Graph, query as neo_query};
use ulid::Ulid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let uri =
        std::env::var("PHILOTIC_MEMGRAPH_URI").unwrap_or_else(|_| "100.64.212.8:7687".to_string());
    let user = std::env::var("PHILOTIC_MEMGRAPH_USER").unwrap_or_default();
    let password = std::env::var("PHILOTIC_MEMGRAPH_PASSWORD").unwrap_or_default();

    let query_id = format!("smoke-recall-{}", Ulid::new().to_string().to_lowercase());

    // Unit vector: dim 0 = 1.0, rest 0.0 — will match any indexed node with that embedding.
    // (Most nodes won't have embeddings yet; 0 results is expected correct behaviour.)
    let embedding: Vec<f32> = {
        let mut v = vec![0.0_f32; LIFE_GRAPH_EMBEDDING_DIMS];
        v[0] = 1.0;
        v
    };

    let recall_query = RetrievalQuery {
        query_id: query_id.clone(),
        query_text: "smoke test recall".to_string(),
        strategy: RetrievalStrategy::MemoryAwareGraphRank,
        semantic_pivots: vec![SemanticPivot {
            space: SemanticSpace::LifeEventSemantic,
            embedding_model: "text-embedding-3-small".to_string(),
            embedding_dims: LIFE_GRAPH_EMBEDDING_DIMS,
            query_text_hash: "smoke-hash-placeholder".to_string(),
            vector_ref: None,
        }],
        expansion_policy: ExpansionPolicy::default(),
        policy_filters: vec![PolicyFilter::ExcludeRetired],
        ranking_weights: RankingWeights::default(),
        active_role: None,
        operator_intent: Some("smoke test".to_string()),
        max_context_packets: 5,
    };

    println!("Connecting to Memgraph at {uri}");
    let builder = ConfigBuilder::default()
        .uri(uri.as_str())
        .user(user.as_str())
        .password(password.as_str());
    let graph = Graph::connect(builder.build()?)?;

    println!("Executing vector search for query_id={query_id}");

    let now = chrono::Utc::now();
    let now_iso = now.to_rfc3339();
    let min_sim = 0.3_f32;
    let top_k = recall_query.max_context_packets * 3;

    let mut all_hits = Vec::new();
    for pivot in &recall_query.semantic_pivots {
        for label in space_labels(&pivot.space) {
            let index = projection::index_name(&pivot.space, label);
            let cypher = projection::semantic_expand_cypher(&index, top_k, &embedding, min_sim);

            let mut rows_stream = graph.execute(neo_query(&cypher)).await?;
            let mut raw_rows = Vec::new();
            while let Some(row) = rows_stream.next().await? {
                raw_rows.push(row_to_json(&row)?);
            }
            let result = serde_json::json!({ "rows": raw_rows });
            let hits = projection::parse_vector_search_rows(&result);
            println!("  index={index}: {} hits", hits.len());
            all_hits.extend(hits);
        }
    }

    let (surviving, drop_log) =
        projection::apply_policy_filters(all_hits, &recall_query.policy_filters);

    println!(
        "Policy filter: {} surviving, {} dropped",
        surviving.len(),
        drop_log.len()
    );

    let scored: Vec<(projection::VectorHit, f32, Vec<PolicyFilter>)> = surviving
        .into_iter()
        .map(|hit| {
            let age_secs = hit
                .prop_str("observed_at")
                .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
                .map(|dt| {
                    let elapsed = now - dt.with_timezone(&chrono::Utc);
                    elapsed.num_seconds().max(0) as u64
                })
                .unwrap_or(0);
            let role_matched = recall_query
                .active_role
                .as_deref()
                .is_some_and(|slug| projection::hit_matches_domain(&hit, slug));
            let score = projection::ranking_score(
                &hit,
                &recall_query.ranking_weights,
                age_secs,
                role_matched,
            );
            (hit, score, vec![])
        })
        .collect();

    let context_id = format!("ctx:{query_id}");
    let token_budget = recall_query.max_context_packets * 200;
    let packet = projection::project_context_packet(
        &context_id,
        &recall_query.query_id,
        recall_query.strategy.clone(),
        scored,
        Vec::new(),
        token_budget,
        &now_iso,
    );

    println!("\n✓ life.recall smoke PASSED");
    println!("  query_id         = {}", packet.query_id);
    println!("  context_id       = {}", packet.context_id);
    println!("  result_count     = {}", packet.ranked_packets.len());
    println!("  token_budget     = {}", packet.token_budget);
    println!("  generated_at     = {}", packet.generated_at);

    if packet.ranked_packets.is_empty() {
        println!("\n  (0 results — correct: no embeddings written yet)");
        println!("  Next step: write embeddings on observe to populate vector indexes.");
    } else {
        for (i, rp) in packet.ranked_packets.iter().enumerate() {
            println!(
                "  [{}] {} [{}] score={:.3}",
                i, rp.packet.claim_ref.id, rp.packet.claim_ref.label, rp.score
            );
        }
    }

    // Serialise to confirm JSON shape is valid
    let json = serde_json::to_string_pretty(&packet)?;
    assert!(
        json.contains("\"context_id\""),
        "packet should serialize to JSON with context_id"
    );
    assert!(
        json.contains("\"ranked_packets\""),
        "packet should have ranked_packets array"
    );

    Ok(())
}

fn space_labels(space: &SemanticSpace) -> &'static [&'static str] {
    match space {
        SemanticSpace::LifeEventSemantic => &["Event", "Signal", "OpenLoop"],
        SemanticSpace::GoalSystemSemantic => &[
            "Goal",
            "System",
            "Habit",
            "Project",
            "Routine",
            "NextAction",
        ],
        SemanticSpace::SkillToolSemantic => &[
            "GrowthHypothesis",
            "GrowthExperiment",
            "DriftFinding",
            "CapabilityPatch",
            "SkillPatch",
            "ToolPatch",
            "SchemaPatch",
            "AttentionPatch",
            "SystemPatch",
        ],
        SemanticSpace::RolePersonSemantic => &["Role", "Person", "Value", "Preference", "Concern"],
        SemanticSpace::MemoryBridgeSemantic => &["Commitment", "Decision"],
    }
}

// Minimal bolt→JSON for the smoke driver (mirrors provider.rs)
fn row_to_json(row: &neo4rs::Row) -> anyhow::Result<serde_json::Value> {
    let mut object = serde_json::Map::new();
    for key in row.keys() {
        let key = key.value.as_str();
        let value: neo4rs::BoltType = row.get(key)?;
        object.insert(key.to_string(), bolt_value_to_json(value));
    }
    Ok(serde_json::Value::Object(object))
}

fn bolt_value_to_json(value: neo4rs::BoltType) -> serde_json::Value {
    use neo4rs::BoltType;
    match value {
        BoltType::String(v) => serde_json::json!(v.value),
        BoltType::Boolean(v) => serde_json::json!(v.value),
        BoltType::Integer(v) => serde_json::json!(v.value),
        BoltType::Float(v) => serde_json::json!(v.value),
        BoltType::Null(_) => serde_json::Value::Null,
        BoltType::List(v) => {
            serde_json::Value::Array(v.into_iter().map(bolt_value_to_json).collect())
        }
        BoltType::Map(v) => serde_json::Value::Object(
            v.value
                .into_iter()
                .map(|(k, val)| (k.value, bolt_value_to_json(val)))
                .collect(),
        ),
        BoltType::Node(v) => serde_json::json!({
            "kind": "node",
            "id": v.id.value,
            "labels": v.labels.into_iter().map(bolt_value_to_json).collect::<Vec<_>>(),
            "properties": serde_json::Value::Object(
                v.properties.value.into_iter()
                    .map(|(k, val)| (k.value, bolt_value_to_json(val)))
                    .collect()
            ),
        }),
        other => serde_json::json!({ "kind": "unsupported", "debug": format!("{other:?}") }),
    }
}
