use data_memorygraphrag::cypher;
/// Live smoke test for life.observe → Memgraph MERGE.
///
/// Run with:
///   PHILOTIC_MEMGRAPH_URI=100.64.212.8:7687 \
///   cargo run -p data-memorygraphrag --example observe_smoke
///
/// Expected: a Signal node with validation_state="proposed" lands in Memgraph.
/// Verify: MATCH (n:Signal {id: "<node_id>"}) RETURN n;
use data_memorygraphrag::{
    AdjudicationStatus, EvidencePacket, GraphRecordRef, LifeObserveInput, ReliabilityBasis,
    SourceKind, SourceRef, SourceReliability, ValidationState,
};
use neo4rs::{ConfigBuilder, Graph, query};
use ulid::Ulid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let uri =
        std::env::var("PHILOTIC_MEMGRAPH_URI").unwrap_or_else(|_| "100.64.212.8:7687".to_string());
    let user = std::env::var("PHILOTIC_MEMGRAPH_USER").unwrap_or_default();
    let password = std::env::var("PHILOTIC_MEMGRAPH_PASSWORD").unwrap_or_default();

    let node_id = format!("smoke-signal-{}", Ulid::new().to_string().to_lowercase());
    let observation_id = format!("obs-{}", Ulid::new().to_string().to_lowercase());
    let packet_id = format!("pkt-{}", Ulid::new().to_string().to_lowercase());

    let input = LifeObserveInput {
        observation_id: observation_id.clone(),
        evidence: EvidencePacket {
            packet_id: packet_id.clone(),
            claim_ref: GraphRecordRef {
                id: node_id.clone(),
                label: "Signal".to_string(),
                datasource: None,
            },
            claim_summary: "Life Graph runner smoke test: Signal node".to_string(),
            source_refs: vec![SourceRef {
                source_id: "agent:memorygraphrag-smoke".to_string(),
                source_kind: SourceKind::RuntimeObservation,
                reliability: SourceReliability {
                    score: 0.9,
                    basis: ReliabilityBasis::DirectObservation,
                },
                uri: None,
                captured_at: None,
            }],
            passage_refs: vec![],
            confidence: 0.85,
            validation_state: ValidationState::Proposed,
            observed_at: Some("2026-06-04T00:00:00Z".to_string()),
            valid_time_range: None,
            source_reliability: 0.9,
            conflict_ids: vec![],
            adjudication_status: AdjudicationStatus::NotNeeded,
            metadata: serde_json::Value::Null,
        },
        proposed_graph_refs: vec![],
        observed_by: None,
        observed_role: None,
        edges: vec![],
        provenance: None,
    };

    let now = chrono::Utc::now().to_rfc3339();
    let compiled = cypher::compile_observe(&input, &now)
        .map_err(|e| anyhow::anyhow!("Cypher compile error: {e}"))?;

    println!("Connecting to Memgraph at {uri}");
    let builder = ConfigBuilder::default()
        .uri(uri.as_str())
        .user(user.as_str())
        .password(password.as_str());

    let graph = Graph::connect(builder.build()?)?;

    println!("Executing MERGE for Signal id={node_id}");

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
    let row = rows.next().await?;

    match &row {
        Some(r) => {
            let returned_id: String = r.get("id").unwrap_or_default();
            let validation_state: String = r.get("validation_state").unwrap_or_default();
            println!("✓ life.observe smoke PASSED");
            println!("  node_id          = {returned_id}");
            println!("  validation_state = {validation_state}");
            println!("  observation_id   = {observation_id}");
            println!("  packet_id        = {packet_id}");
            println!();
            println!("Verify with:");
            println!("  MATCH (n:Signal {{id: \"{returned_id}\"}}) RETURN n;");

            assert_eq!(returned_id, node_id, "returned node id should match");
            assert_eq!(
                validation_state, "proposed",
                "validation_state should be 'proposed'"
            );
        }
        None => {
            anyhow::bail!("MERGE returned no rows — node was not written");
        }
    }

    Ok(())
}
