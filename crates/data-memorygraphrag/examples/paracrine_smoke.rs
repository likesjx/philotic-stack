/// Live smoke test: paracrine_signal → attention steward decision → Memgraph persistence.
///
/// Run with:
///   PHILOTIC_MEMGRAPH_URI=100.64.212.8:7687 \
///   cargo run -p data-memorygraphrag --example paracrine_smoke
///
/// Expected: attention_observer maps the signal to a LifeObserveInput, compile_observe
/// produces valid Cypher, and the Signal node (or StewardshipInstruction node) is written
/// to vps-jane Memgraph. Verify with:
///   MATCH (n:Signal {id: "signal:paracrine:<signal_id>"}) RETURN n
use ansible_mesh_core::attention_steward::{
    AttentionStewardPolicy, AttentionStewardResponse, AttentionStewardSignal,
};
use data_memorygraphrag::attention_observer;
use data_memorygraphrag::cypher;
use neo4rs::{ConfigBuilder, Graph, query as neo_query};
use serde_json::json;
use ulid::Ulid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let uri = std::env::var("PHILOTIC_MEMGRAPH_URI")
        .unwrap_or_else(|_| "100.64.212.8:7687".to_string());
    let user = std::env::var("PHILOTIC_MEMGRAPH_USER").unwrap_or_default();
    let password = std::env::var("PHILOTIC_MEMGRAPH_PASSWORD").unwrap_or_default();

    let signal_id = format!("cron:smoke-paracrine:{}", Ulid::new().to_string().to_lowercase());

    // --- Slice A: RecordObservation → Signal node ---
    let observe_signal = AttentionStewardSignal::from_value(json!({
        "signal_id": signal_id,
        "signal_type": "open_loop_staleness",
        "scope": "personal",
        "source_hotel": "vps-jane-aiua-01",
        "target_role_type": "attention-steward",
        "subject_refs": ["lifegraph:open_loop:smoke-test"],
        "cadence": "daily",
        "priority": "medium",
        "observed_at": "2026-06-04T20:00:00Z",
        "expires_at": null,
        "payload_summary": "Smoke paracrine: open loop staleness signal for attention steward.",
        "policy_tags": ["observe_only", "adhd-support"]
    }))?;

    let policy = AttentionStewardPolicy::default();
    let decision = policy.evaluate_now(&observe_signal);
    assert_eq!(
        decision.response,
        AttentionStewardResponse::RecordObservation,
        "smoke signal should produce RecordObservation"
    );

    let now_iso = chrono::Utc::now().to_rfc3339();
    let observe_input = attention_observer::decision_to_observe_input(&decision, &observe_signal, &now_iso)
        .expect("RecordObservation should produce LifeObserveInput");

    assert_eq!(observe_input.evidence.claim_ref.label, "Signal");

    let compiled =
        cypher::compile_observe(&observe_input, &now_iso).expect("should compile Signal Cypher");

    println!("Connecting to Memgraph at {uri}");
    let builder = ConfigBuilder::default()
        .uri(uri.as_str())
        .user(user.as_str())
        .password(password.as_str());
    let graph = Graph::connect(builder.build()?)?;

    let q = neo_query(&compiled.query)
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
    let first = rows.next().await?;
    let written_id = first
        .as_ref()
        .and_then(|r| r.get::<String>("id").ok())
        .unwrap_or_else(|| compiled.node_id.clone());

    println!("✓ Signal node written: id={written_id}");

    // Verify node is readable back
    let verify_q = neo_query(&format!(
        "MATCH (n:Signal {{id: $id}}) RETURN n.validation_state AS vs, n.claim_summary AS cs"
    ))
    .param("id", compiled.node_id.as_str());

    let mut verify_rows = graph.execute(verify_q).await?;
    let verify_row = verify_rows.next().await?.expect("Signal node should be readable");
    let vs = verify_row.get::<String>("vs").unwrap_or_default();
    let cs = verify_row.get::<String>("cs").unwrap_or_default();

    assert_eq!(vs, "proposed", "validation_state should be proposed");
    assert!(!cs.is_empty(), "claim_summary should be set");
    println!("✓ Signal node verified: validation_state={vs}");

    // --- Slice B: ProposeSilEntry → StewardshipInstruction node ---
    let sil_signal_id =
        format!("cron:smoke-sil:{}", Ulid::new().to_string().to_lowercase());
    let sil_signal = AttentionStewardSignal::from_value(json!({
        "signal_id": sil_signal_id,
        "signal_type": "re_entry_hint",
        "scope": "personal",
        "source_hotel": "vps-jane-aiua-01",
        "target_role_type": "attention-steward",
        "subject_refs": [],
        "cadence": "weekly",
        "priority": "low",
        "observed_at": "2026-06-04T20:00:00Z",
        "expires_at": null,
        "payload_summary": "Smoke: operator re-entered domain after 5-day gap — new pattern.",
        "policy_tags": ["new_pattern"]
    }))?;

    let sil_decision = policy.evaluate_now(&sil_signal);
    assert_eq!(
        sil_decision.response,
        AttentionStewardResponse::ProposeSilEntry,
        "new_pattern signal should produce ProposeSilEntry"
    );

    let sil_input = attention_observer::decision_to_observe_input(&sil_decision, &sil_signal, &now_iso)
        .expect("ProposeSilEntry should produce LifeObserveInput");

    assert_eq!(sil_input.evidence.claim_ref.label, "StewardshipInstruction");

    let sil_compiled =
        cypher::compile_observe(&sil_input, &now_iso).expect("should compile StewardshipInstruction Cypher");

    let sil_q = neo_query(&sil_compiled.query)
        .param("id", sil_compiled.node_id.as_str())
        .param("created_at", sil_compiled.created_at.as_str())
        .param("source_membrane", sil_compiled.source_membrane.as_str())
        .param("provenance", sil_compiled.provenance.as_str())
        .param("confidence", sil_compiled.confidence)
        .param("validation_state", sil_compiled.validation_state.as_str())
        .param("observed_at", sil_compiled.observed_at.as_str())
        .param("claim_summary", sil_compiled.claim_summary.as_str())
        .param("observation_id", sil_compiled.observation_id.as_str())
        .param("packet_id", sil_compiled.packet_id.as_str());

    let mut sil_rows = graph.execute(sil_q).await?;
    let sil_first = sil_rows.next().await?;
    let sil_written_id = sil_first
        .as_ref()
        .and_then(|r| r.get::<String>("id").ok())
        .unwrap_or_else(|| sil_compiled.node_id.clone());

    println!("✓ StewardshipInstruction node written: id={sil_written_id}");

    // Verify StewardshipInstruction is readable
    let sil_verify_q = neo_query(
        "MATCH (n:StewardshipInstruction {id: $id}) RETURN n.validation_state AS vs",
    )
    .param("id", sil_compiled.node_id.as_str());

    let mut sil_verify_rows = graph.execute(sil_verify_q).await?;
    let sil_verify_row = sil_verify_rows
        .next()
        .await?
        .expect("StewardshipInstruction node should be readable");
    let sil_vs = sil_verify_row.get::<String>("vs").unwrap_or_default();
    assert_eq!(sil_vs, "proposed");
    println!("✓ StewardshipInstruction node verified: validation_state={sil_vs}");

    println!("\n✓ life-graph-attention-steward smoke PASSED");
    println!("  Signal node              = {written_id}");
    println!("  StewardshipInstruction   = {sil_written_id}");

    Ok(())
}
