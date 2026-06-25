use anyhow::{Context, Result};
use async_trait::async_trait;
use data_memorygraphrag::LIFE_GRAPH_EMBEDDING_DIMS;
use data_memorygraphrag::cypher;
use data_memorygraphrag::projection;
use data_memorygraphrag::{
    AdjudicationStatus, ConflictHandoff, EvidencePacket, GraphRecordRef, LifeCommitInput,
    LifeGraphToolRequest, LifeObserveInput, LifePatchProposalInput, LifeResolveInput,
    MemoryGraphRagRunner, PatchKind, PatchRisk, PolicyFilter, ReliabilityBasis,
    RetrievalFeedbackInput, RetrievalFeedbackRating, RetrievalQuery, RunnerConfig,
    RunnerPlanTarget, SemanticSpace, SourceKind, SourceRef, SourceReliability, ValidationState,
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
            "life.recall.feedback" => self.handle_recall_feedback(task).await,
            "life.commit" => self.handle_commit(task).await,
            "life.resolve" | "life.conflict.resolve" => self.handle_resolve(task).await,
            "life.conflict" | "life.conflict.handle" => self.handle_conflict(task).await,
            "life.patch.propose" => self.handle_patch_propose(task).await,
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

        // Embed-on-write: compute embedding for the claim_summary and write it back.
        // Explicit error on dim mismatch — a wrong embedding silently breaks retrieval.
        let embed_status = match embed_text(&compiled.claim_summary).await {
            Ok((vector, model_gen)) => {
                if vector.len() != LIFE_GRAPH_EMBEDDING_DIMS {
                    let msg = format!(
                        "embed-on-write: sidecar returned {}d but Life Graph requires {}d; \
                         check PHILOTIC_ONNX_EMBED_REPO on the hotel",
                        vector.len(),
                        LIFE_GRAPH_EMBEDDING_DIMS
                    );
                    warn!("{msg}");
                    "wrong_dim"
                } else {
                    let embed_cypher = format!(
                        "MATCH (n:{} {{id: $id}}) \
                         SET n.embedding = $vec, \
                             n.embedding_model_gen = $gen, \
                             n.embedding_dims = {}, \
                             n.embedding_updated_at = $now, \
                             n.embedding_space = $space \
                         RETURN n.embedding_dims AS embedding_dims, \
                                size(n.embedding) AS embedding_len",
                        compiled.label, LIFE_GRAPH_EMBEDDING_DIMS
                    );
                    let space = projection::embedding_space_for_label(&compiled.label)
                        .unwrap_or("life_event_semantic");
                    let vector_param: Vec<f64> = vector.iter().map(|v| f64::from(*v)).collect();
                    match graph
                        .execute(
                            query(&embed_cypher)
                                .param("id", compiled.node_id.as_str())
                                .param("vec", vector_param)
                                .param("gen", model_gen.as_str())
                                .param("now", now.as_str())
                                .param("space", space),
                        )
                        .await
                    {
                        Ok(mut rows) => match rows.next().await {
                            Ok(Some(row)) => {
                                let dims = row
                                    .get::<i64>("embedding_dims")
                                    .unwrap_or(LIFE_GRAPH_EMBEDDING_DIMS as i64);
                                let len = row
                                    .get::<i64>("embedding_len")
                                    .unwrap_or(LIFE_GRAPH_EMBEDDING_DIMS as i64);
                                if dims == LIFE_GRAPH_EMBEDDING_DIMS as i64
                                    && len == LIFE_GRAPH_EMBEDDING_DIMS as i64
                                {
                                    info!(
                                        node_id = %node_id,
                                        model_gen = %model_gen,
                                        dims,
                                        len,
                                        "embed-on-write OK"
                                    );
                                    "ok"
                                } else {
                                    warn!(
                                        node_id = %node_id,
                                        dims,
                                        len,
                                        "embed-on-write returned unexpected metadata"
                                    );
                                    "write_mismatch"
                                }
                            }
                            Ok(None) => {
                                warn!(
                                    node_id = %compiled.node_id,
                                    "embed-on-write matched no Life Graph node"
                                );
                                "write_missed"
                            }
                            Err(e) => {
                                warn!("embed-on-write result read failed: {e}");
                                "write_failed"
                            }
                        },
                        Err(e) => {
                            warn!("embed-on-write SET failed: {e}");
                            "write_failed"
                        }
                    }
                }
            }
            Err(e) => {
                warn!("embed-on-write skipped: {e}");
                "sidecar_unavailable"
            }
        };

        Ok(ProviderOutput::ResultSet(json!({
            "status": "proposed",
            "node_id": node_id,
            "label": compiled.label,
            "observation_id": compiled.observation_id,
            "packet_id": compiled.packet_id,
            "validation_state": compiled.validation_state,
            "embed_status": embed_status,
        })))
    }

    async fn handle_recall(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        let query_val: RetrievalQuery = serde_json::from_value(task.parameters.clone())
            .context("failed to parse life.recall parameters as RetrievalQuery")?;
        let named_strategy = NamedRecallStrategy::from_task(task);
        if !matches!(named_strategy, NamedRecallStrategy::CommitmentsApproaching) {
            self.runner
                .plan(LifeGraphToolRequest::LifeRecall(query_val.clone()))
                .map_err(|e| anyhow::anyhow!("life.recall plan validation failed: {e}"))?;
        }

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

        // Auto-embed query_text when the caller didn't supply a pre-computed
        // embedding.  This lets orchestrator agents call life.recall with just
        // { query_text, named_strategy } without needing access to the ONNX sidecar.
        let embedding = if embedding.is_empty()
            && !matches!(named_strategy, NamedRecallStrategy::CommitmentsApproaching)
        {
            let query_text = &query_val.query_text;
            if query_text.is_empty() {
                return Ok(ProviderOutput::ResultSet(json!({
                    "status": "missing_embedding",
                    "detail": "life.recall requires either an inline 'embedding' array or a non-empty 'query_text' to auto-embed",
                })));
            }
            match embed_text(query_text).await {
                Ok((auto_vec, _)) => auto_vec,
                Err(e) => {
                    warn!("life.recall auto-embed failed, returning empty result: {e}");
                    return Ok(ProviderOutput::ResultSet(json!({
                        "status": "embed_failed",
                        "detail": format!("auto-embedding failed: {e}"),
                    })));
                }
            }
        } else {
            embedding
        };

        let top_k = query_val.max_context_packets * 3;
        let min_similarity = 0.3_f32;
        let now = chrono::Utc::now();
        let now_iso = now.to_rfc3339();

        let mut all_hits = Vec::new();

        match named_strategy {
            NamedRecallStrategy::OpenLoopsByContext => {
                self.extend_vector_hits(
                    &mut all_hits,
                    SemanticSpace::LifeEventSemantic,
                    &["OpenLoop"],
                    top_k.max(10),
                    0.4,
                    &embedding,
                )
                .await?;
            }
            NamedRecallStrategy::GoalsAndNextActions => {
                self.extend_vector_hits(
                    &mut all_hits,
                    SemanticSpace::GoalSystemSemantic,
                    &["Goal"],
                    top_k.max(8),
                    0.35,
                    &embedding,
                )
                .await?;
            }
            NamedRecallStrategy::CommitmentsApproaching => {
                let due_within_hours = task
                    .parameters
                    .get("due_within_hours")
                    .and_then(Value::as_u64)
                    .unwrap_or(72);
                let deadline =
                    (now + chrono::Duration::hours(due_within_hours as i64)).to_rfc3339();
                let cypher = commitments_approaching_cypher(&deadline);
                let result = self.execute_cypher(&cypher).await?;
                all_hits.extend(projection::parse_vector_search_rows(&result));

                if all_hits.len() < 3 && !embedding.is_empty() {
                    self.extend_vector_hits(
                        &mut all_hits,
                        SemanticSpace::MemoryBridgeSemantic,
                        &["Commitment"],
                        5,
                        0.3,
                        &embedding,
                    )
                    .await?;
                }
            }
            NamedRecallStrategy::ReEntryContext => {
                self.extend_vector_hits(
                    &mut all_hits,
                    SemanticSpace::LifeEventSemantic,
                    &["Event"],
                    6,
                    0.4,
                    &embedding,
                )
                .await?;
                self.extend_vector_hits(
                    &mut all_hits,
                    SemanticSpace::GoalSystemSemantic,
                    &["Goal"],
                    5,
                    0.35,
                    &embedding,
                )
                .await?;
            }
            NamedRecallStrategy::CrossDomainEntanglement => {
                let domain_a_embedding = embedding_from_key(&task.parameters, "domain_a_embedding")
                    .unwrap_or_else(|| embedding.clone());
                let domain_b_embedding = embedding_from_key(&task.parameters, "domain_b_embedding")
                    .unwrap_or_else(|| embedding.clone());
                self.extend_vector_hits(
                    &mut all_hits,
                    SemanticSpace::LifeEventSemantic,
                    &["Signal"],
                    8,
                    0.4,
                    &domain_a_embedding,
                )
                .await?;
                self.extend_vector_hits(
                    &mut all_hits,
                    SemanticSpace::GoalSystemSemantic,
                    &["Goal"],
                    8,
                    0.4,
                    &domain_b_embedding,
                )
                .await?;
            }
            NamedRecallStrategy::SemanticPivot => {
                for pivot in &query_val.semantic_pivots {
                    for label in projection::labels_for_space(&pivot.space) {
                        self.extend_vector_hits(
                            &mut all_hits,
                            pivot.space.clone(),
                            &[*label],
                            top_k,
                            min_similarity,
                            &embedding,
                        )
                        .await?;
                    }
                }
            }
        }

        let filters = &query_val.policy_filters;
        let weights = &query_val.ranking_weights;
        let mut scored = score_hits(all_hits, filters, weights, &now);

        let fallback_used = if scored.is_empty() {
            let fallback_labels = named_strategy.fallback_labels(&query_val);
            if fallback_labels.is_empty() {
                false
            } else {
                let limit = query_val.max_context_packets.max(1) * 3;
                let cypher = raw_recall_fallback_cypher(&fallback_labels, limit);
                let result = self.execute_cypher(&cypher).await?;
                let fallback_hits = projection::parse_vector_search_rows(&result);
                scored = score_hits(fallback_hits, filters, weights, &now);
                !scored.is_empty()
            }
        } else {
            false
        };
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
            fallback_used,
            "life.recall: context packet projected"
        );

        let packet_json =
            serde_json::to_value(&packet).context("failed to serialize RetrievalContextPacket")?;

        Ok(ProviderOutput::ResultSet(json!({
            "status": "ok",
            "named_strategy": named_strategy.as_str(),
            "fallback_used": fallback_used,
            "context_packet": packet_json,
        })))
    }

    async fn handle_commit(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        let input: LifeCommitInput = serde_json::from_value(task.parameters.clone())
            .context("failed to parse life.commit parameters as LifeCommitInput")?;
        let plan = self
            .runner
            .plan(LifeGraphToolRequest::LifeCommit(input.clone()))
            .map_err(|e| anyhow::anyhow!("life.commit plan validation failed: {e}"))?;
        if !plan.allowed() {
            return Ok(ProviderOutput::ResultSet(json!({
                "status": "blocked",
                "reasons": plan.blocked_reasons,
            })));
        }

        let now = chrono::Utc::now().to_rfc3339();
        let compiled = cypher::compile_commit(&input, &now)
            .map_err(|e| anyhow::anyhow!("life.commit Cypher compilation failed: {e}"))?;
        let graph = self.connect().await?;
        let mut rows = graph
            .execute(
                query(&compiled.query)
                    .param("id", compiled.node_id.as_str())
                    .param("confirmed_at", compiled.confirmed_at.as_str())
                    .param("confidence", compiled.confidence)
                    .param("claim_summary", compiled.claim_summary.as_str())
                    .param("packet_id", compiled.packet_id.as_str()),
            )
            .await?;
        let first_row = rows.next().await?;
        let node_id = first_row
            .as_ref()
            .and_then(|r| r.get::<String>("id").ok())
            .unwrap_or_else(|| compiled.node_id.clone());

        Ok(ProviderOutput::ResultSet(json!({
            "status": "committed",
            "node_id": node_id,
            "label": compiled.label,
            "packet_id": compiled.packet_id,
            "validation_state": "confirmed",
        })))
    }

    async fn handle_conflict(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        let handoff_value = task
            .parameters
            .get("handoff")
            .cloned()
            .unwrap_or_else(|| task.parameters.clone());
        let handoff: ConflictHandoff = serde_json::from_value(handoff_value)
            .context("failed to parse life.conflict parameters as ConflictHandoff")?;
        let now = chrono::Utc::now().to_rfc3339();
        let compiled = cypher::compile_conflict_handoff(&handoff, &now)
            .map_err(|e| anyhow::anyhow!("life.conflict Cypher compilation failed: {e}"))?;
        self.execute_conflict_cypher(&compiled).await?;

        Ok(ProviderOutput::ResultSet(json!({
            "status": "open",
            "handoff_id": compiled.handoff_id,
            "conflict_id": compiled.conflict_id,
        })))
    }

    async fn handle_resolve(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        let input: LifeResolveInput = serde_json::from_value(task.parameters.clone())
            .context("failed to parse life.resolve parameters as LifeResolveInput")?;
        let plan = self
            .runner
            .plan(LifeGraphToolRequest::LifeResolve(input.clone()))
            .map_err(|e| anyhow::anyhow!("life.resolve plan validation failed: {e}"))?;
        if !plan.allowed() {
            return Ok(ProviderOutput::ResultSet(json!({
                "status": "blocked",
                "reasons": plan.blocked_reasons,
            })));
        }

        let now = chrono::Utc::now().to_rfc3339();
        let compiled = cypher::compile_resolve(&input, &now)
            .map_err(|e| anyhow::anyhow!("life.resolve Cypher compilation failed: {e}"))?;
        self.execute_conflict_cypher(&compiled).await?;

        let muninn_steps: Vec<_> = plan
            .steps
            .into_iter()
            .filter(|step| step.target == RunnerPlanTarget::Muninn)
            .collect();

        Ok(ProviderOutput::ResultSet(json!({
            "status": "resolved",
            "handoff_id": compiled.handoff_id,
            "conflict_id": compiled.conflict_id,
            "muninn_handoff_required": !muninn_steps.is_empty(),
            "muninn_steps": muninn_steps,
        })))
    }

    async fn handle_patch_propose(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        let input: LifePatchProposalInput = serde_json::from_value(task.parameters.clone())
            .context("failed to parse life.patch.propose parameters as LifePatchProposalInput")?;
        let plan = self
            .runner
            .plan(LifeGraphToolRequest::LifePatchPropose(input.clone()))
            .map_err(|e| anyhow::anyhow!("life.patch.propose plan validation failed: {e}"))?;
        let now = chrono::Utc::now().to_rfc3339();
        let compiled = cypher::compile_patch_proposal(&input, &now)
            .map_err(|e| anyhow::anyhow!("life.patch.propose Cypher compilation failed: {e}"))?;

        let graph = self.connect().await?;
        let mut rows = graph
            .execute(
                query(&compiled.query)
                    .param("patch_id", compiled.patch_id.as_str())
                    .param("patch_kind", compiled.patch_kind.as_str())
                    .param("summary", compiled.summary.as_str())
                    .param("rationale", compiled.rationale.as_str())
                    .param("risk", compiled.risk.as_str())
                    .param("status", compiled.status.as_str())
                    .param("proposed_at", compiled.proposed_at.as_str())
                    .param("patch_json", compiled.patch_json.as_str()),
            )
            .await?;
        let _ = rows.next().await?;

        Ok(ProviderOutput::ResultSet(json!({
            "status": if plan.requires_operator { "awaiting_operator" } else { "proposed" },
            "patch_id": compiled.patch_id,
            "label": compiled.label,
            "requires_operator": plan.requires_operator,
            "blocked_reasons": plan.blocked_reasons,
        })))
    }

    async fn handle_recall_feedback(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        let input: RetrievalFeedbackInput = serde_json::from_value(task.parameters.clone())
            .context("failed to parse life.recall.feedback parameters as RetrievalFeedbackInput")?;
        let plan = self
            .runner
            .plan(LifeGraphToolRequest::LifeRecallFeedback(input.clone()))
            .map_err(|e| anyhow::anyhow!("life.recall.feedback plan validation failed: {e}"))?;

        let growth_evaluation = plan
            .steps
            .first()
            .and_then(|step| step.payload.get("growth_evaluation"))
            .cloned()
            .unwrap_or_else(|| json!({}));

        let now = chrono::Utc::now().to_rfc3339();
        let compiled = cypher::compile_recall_feedback(&input, &growth_evaluation, &now)
            .map_err(|e| anyhow::anyhow!("life.recall.feedback Cypher compilation failed: {e}"))?;

        let graph = self.connect().await?;
        let mut q = query(&compiled.query)
            .param("feedback_id", compiled.feedback_id.as_str())
            .param("packet_id", compiled.packet_id.as_str())
            .param("rating", compiled.rating.as_str())
            .param("query_summary", compiled.query_summary.as_str())
            .param("note", compiled.note.as_str())
            .param("candidate_count", compiled.candidate_count)
            .param(
                "connected_candidate_count",
                compiled.connected_candidate_count,
            )
            .param("feedback_json", compiled.feedback_json.as_str())
            .param("evaluation_json", compiled.evaluation_json.as_str())
            .param("observed_at", compiled.observed_at.as_str());
        if let Some(ratio) = compiled.connectivity_ratio {
            q = q.param("connectivity_ratio", ratio);
        } else {
            q = q.param("connectivity_ratio", 0.0_f64);
        }
        let mut rows = graph.execute(q).await?;
        let _ = rows.next().await?;

        let generated_patch = recall_feedback_patch_proposal(&input);
        let generated_patch_summary = if let Some(patch) = &generated_patch {
            let compiled_patch = cypher::compile_patch_proposal(patch, &now).map_err(|e| {
                anyhow::anyhow!("life.recall.feedback patch Cypher compilation failed: {e}")
            })?;
            let mut patch_rows = graph
                .execute(
                    query(&compiled_patch.query)
                        .param("patch_id", compiled_patch.patch_id.as_str())
                        .param("patch_kind", compiled_patch.patch_kind.as_str())
                        .param("summary", compiled_patch.summary.as_str())
                        .param("rationale", compiled_patch.rationale.as_str())
                        .param("risk", compiled_patch.risk.as_str())
                        .param("status", compiled_patch.status.as_str())
                        .param("proposed_at", compiled_patch.proposed_at.as_str())
                        .param("patch_json", compiled_patch.patch_json.as_str()),
                )
                .await?;
            let _ = patch_rows.next().await?;
            Some(json!({
                "patch_id": compiled_patch.patch_id,
                "patch_kind": compiled_patch.patch_kind,
                "label": compiled_patch.label,
                "risk": compiled_patch.risk,
                "status": compiled_patch.status,
            }))
        } else {
            None
        };

        let improvement_steps: Vec<_> = plan
            .steps
            .iter()
            .filter(|step| step.action == "life.graph.improvement_candidates")
            .cloned()
            .collect();

        Ok(ProviderOutput::ResultSet(json!({
            "status": if plan.requires_operator { "awaiting_operator" } else { "recorded" },
            "feedback_id": compiled.feedback_id,
            "packet_id": compiled.packet_id,
            "rating": compiled.rating,
            "connectivity_ratio": compiled.connectivity_ratio,
            "growth_evaluation": growth_evaluation,
            "improvement_steps": improvement_steps,
            "generated_patch": generated_patch_summary,
            "requires_operator": plan.requires_operator,
        })))
    }

    async fn execute_conflict_cypher(&self, compiled: &cypher::ConflictCypher) -> Result<()> {
        let graph = self.connect().await?;
        let mut q = query(&compiled.query)
            .param("handoff_id", compiled.handoff_id.as_str())
            .param("conflict_id", compiled.conflict_id.as_str())
            .param("summary", compiled.summary.as_str())
            .param("status", compiled.status.as_str())
            .param("updated_at", compiled.updated_at.as_str())
            .param("handoff_json", compiled.handoff_json.as_str());
        if let Some(summary) = &compiled.resolution_summary {
            q = q.param("resolution_summary", summary.as_str());
        }
        let mut rows = graph.execute(q).await?;
        let _ = rows.next().await?;
        Ok(())
    }

    async fn extend_vector_hits(
        &self,
        all_hits: &mut Vec<projection::VectorHit>,
        space: SemanticSpace,
        labels: &[&str],
        top_k: usize,
        min_similarity: f32,
        embedding: &[f32],
    ) -> Result<()> {
        if embedding.is_empty() {
            return Ok(());
        }
        for label in labels {
            let index = projection::index_name(&space, label);
            let cypher =
                projection::semantic_expand_cypher(&index, top_k, embedding, min_similarity);
            let result = self.execute_cypher(&cypher).await?;
            all_hits.extend(projection::parse_vector_search_rows(&result));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NamedRecallStrategy {
    SemanticPivot,
    OpenLoopsByContext,
    GoalsAndNextActions,
    CommitmentsApproaching,
    ReEntryContext,
    CrossDomainEntanglement,
}

impl NamedRecallStrategy {
    fn from_task(task: &DatasourceTask) -> Self {
        let raw = task
            .parameters
            .get("named_strategy")
            .or_else(|| task.parameters.get("strategy_name"))
            .or_else(|| task.parameters.get("operator_intent"))
            .and_then(Value::as_str)
            .unwrap_or("");

        match raw {
            "open_loops_by_context" => Self::OpenLoopsByContext,
            "goals_and_next_actions" => Self::GoalsAndNextActions,
            "commitments_approaching" => Self::CommitmentsApproaching,
            "re_entry_context" => Self::ReEntryContext,
            "cross_domain_entanglement" => Self::CrossDomainEntanglement,
            _ => Self::SemanticPivot,
        }
    }

    fn fallback_labels(self, query: &RetrievalQuery) -> Vec<&'static str> {
        match self {
            Self::SemanticPivot => query
                .semantic_pivots
                .iter()
                .flat_map(|pivot| projection::labels_for_space(&pivot.space).iter().copied())
                .collect(),
            Self::OpenLoopsByContext => vec!["OpenLoop"],
            Self::GoalsAndNextActions => {
                vec![
                    "Goal",
                    "Habit",
                    "System",
                    "Project",
                    "Routine",
                    "NextAction",
                ]
            }
            Self::CommitmentsApproaching => vec!["Commitment"],
            Self::ReEntryContext => vec!["OpenLoop", "Goal", "Habit", "System", "Role"],
            Self::CrossDomainEntanglement => vec!["Signal", "Goal"],
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::SemanticPivot => "semantic_pivot",
            Self::OpenLoopsByContext => "open_loops_by_context",
            Self::GoalsAndNextActions => "goals_and_next_actions",
            Self::CommitmentsApproaching => "commitments_approaching",
            Self::ReEntryContext => "re_entry_context",
            Self::CrossDomainEntanglement => "cross_domain_entanglement",
        }
    }
}

fn score_hits(
    hits: Vec<projection::VectorHit>,
    filters: &[PolicyFilter],
    weights: &data_memorygraphrag::RankingWeights,
    now: &chrono::DateTime<chrono::Utc>,
) -> Vec<(projection::VectorHit, f32, Vec<PolicyFilter>)> {
    let (surviving, _drop_log) = projection::apply_policy_filters(hits, filters);
    surviving
        .into_iter()
        .map(|hit| {
            let age_secs = compute_age_secs(hit.prop_str("observed_at"), now);
            let score = projection::ranking_score(&hit, weights, age_secs);
            (hit, score, Vec::new())
        })
        .collect()
}

fn raw_recall_fallback_cypher(labels: &[&str], limit: usize) -> String {
    let labels = labels
        .iter()
        .map(|label| format!("'{label}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        concat!(
            "MATCH (n) ",
            "WHERE any(label IN labels(n) WHERE label IN [{labels}]) ",
            "AND coalesce(n.validation_state, 'inferred') <> 'retired' ",
            "AND coalesce(n.status, '') <> 'retired' ",
            "AND coalesce(n.status, '') <> 'done' ",
            "RETURN n AS node, 0.25 AS similarity ",
            "ORDER BY coalesce(n.observed_at, n.created_at, '') DESC ",
            "LIMIT {limit}"
        ),
        labels = labels,
        limit = limit
    )
}

fn embedding_from_key(parameters: &Value, key: &str) -> Option<Vec<f32>> {
    parameters.get(key).and_then(Value::as_array).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_f64().map(|f| f as f32))
            .collect()
    })
}

fn commitments_approaching_cypher(deadline: &str) -> String {
    format!(
        concat!(
            "MATCH (c:Commitment) ",
            "WHERE c.status = 'open' ",
            "AND c.due_at IS NOT NULL ",
            "AND c.due_at <= '{deadline}' ",
            "AND c.validation_state <> 'retired' ",
            "RETURN c AS node, 1.0 AS similarity ",
            "ORDER BY c.due_at ASC LIMIT 10"
        ),
        deadline = deadline
    )
}

fn recall_feedback_patch_proposal(
    input: &RetrievalFeedbackInput,
) -> Option<LifePatchProposalInput> {
    let (patch_kind, risk, summary, rationale) = match input.rating {
        RetrievalFeedbackRating::Useful => return None,
        RetrievalFeedbackRating::Disconnected => (
            PatchKind::SystemPatch,
            PatchRisk::Low,
            "Add bridge/ranking maintenance for disconnected LifeGraph recall.".to_string(),
            "Recall returned candidates that were not connected enough to the active context; propose bridge-building or ranking maintenance grounded in feedback.".to_string(),
        ),
        RetrievalFeedbackRating::Missing => (
            PatchKind::SystemPatch,
            PatchRisk::Low,
            "Add capture or bridge maintenance for missing LifeGraph context.".to_string(),
            "Recall missed expected context; propose capture, bridge, or ontology-gap review without confirming new facts.".to_string(),
        ),
        RetrievalFeedbackRating::Noisy => (
            PatchKind::SystemPatch,
            PatchRisk::Low,
            "Dampen noisy LifeGraph recall paths.".to_string(),
            "Recall included noisy candidates; propose ranking or bridge dampening for low-value hubs.".to_string(),
        ),
        RetrievalFeedbackRating::Stale => (
            PatchKind::SystemPatch,
            PatchRisk::Low,
            "Review stale LifeGraph recall facts.".to_string(),
            "Recall surfaced stale facts; propose stale-marker or confirmation review before reuse.".to_string(),
        ),
        RetrievalFeedbackRating::Overconfident => (
            PatchKind::AttentionPatch,
            PatchRisk::Medium,
            "Require confirmation for overconfident LifeGraph recall.".to_string(),
            "Recall presented inferred context too strongly; require operator confirmation before reinforcing this retrieval pattern.".to_string(),
        ),
    };

    let evidence = if input.evidence_packets.is_empty() {
        vec![feedback_signal_evidence(input)]
    } else {
        input.evidence_packets.clone()
    };

    Some(LifePatchProposalInput {
        patch_id: format!(
            "patch:recall-feedback:{}",
            input.feedback_id.replace(':', "-")
        ),
        patch_kind,
        summary,
        rationale,
        evidence_packets: evidence,
        risk,
        operator_approved: false,
    })
}

fn feedback_signal_evidence(input: &RetrievalFeedbackInput) -> EvidencePacket {
    EvidencePacket {
        packet_id: format!("evidence:{}", input.feedback_id),
        claim_ref: GraphRecordRef {
            id: input.feedback_id.clone(),
            label: "Signal".into(),
            datasource: Some("life-graph".into()),
        },
        claim_summary: format!(
            "LifeGraph recall feedback {:?} for packet {}.",
            input.rating, input.packet_id
        ),
        source_refs: vec![SourceRef {
            source_id: "agent:memorygraphrag".into(),
            source_kind: SourceKind::RuntimeObservation,
            reliability: SourceReliability {
                score: 1.0,
                basis: ReliabilityBasis::DirectObservation,
            },
            uri: None,
            captured_at: None,
        }],
        passage_refs: vec![],
        confidence: 1.0,
        validation_state: ValidationState::Confirmed,
        observed_at: None,
        valid_time_range: None,
        source_reliability: 1.0,
        conflict_ids: vec![],
        adjudication_status: AdjudicationStatus::NotNeeded,
        metadata: json!({
            "packet_id": input.packet_id,
            "query_summary": input.query_summary,
            "rating": input.rating,
            "connectivity_ratio": input.connectivity_ratio(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use datasource::controller::TaskKind;

    fn task_with_params(parameters: Value) -> DatasourceTask {
        DatasourceTask {
            kind: TaskKind::Custom("life.recall".into()),
            provider: None,
            db: None,
            graph_id: None,
            query: None,
            parameters,
            identity: json!({}),
        }
    }

    #[test]
    fn named_recall_strategy_dispatches_all_documented_patterns() {
        let cases = [
            (
                "open_loops_by_context",
                NamedRecallStrategy::OpenLoopsByContext,
            ),
            (
                "goals_and_next_actions",
                NamedRecallStrategy::GoalsAndNextActions,
            ),
            (
                "commitments_approaching",
                NamedRecallStrategy::CommitmentsApproaching,
            ),
            ("re_entry_context", NamedRecallStrategy::ReEntryContext),
            (
                "cross_domain_entanglement",
                NamedRecallStrategy::CrossDomainEntanglement,
            ),
        ];

        for (name, expected) in cases {
            let task = task_with_params(json!({ "named_strategy": name }));
            assert_eq!(NamedRecallStrategy::from_task(&task), expected);
        }
    }

    #[test]
    fn operator_intent_dispatches_named_recall_strategy() {
        let task = task_with_params(json!({ "operator_intent": "goals_and_next_actions" }));
        assert_eq!(
            NamedRecallStrategy::from_task(&task),
            NamedRecallStrategy::GoalsAndNextActions
        );
    }

    #[test]
    fn unknown_named_recall_strategy_falls_back_to_semantic_pivot() {
        let task = task_with_params(json!({ "named_strategy": "surprise_me" }));
        assert_eq!(
            NamedRecallStrategy::from_task(&task),
            NamedRecallStrategy::SemanticPivot
        );
    }

    fn feedback_input(rating: RetrievalFeedbackRating) -> RetrievalFeedbackInput {
        RetrievalFeedbackInput {
            feedback_id: "feedback:recall:1".into(),
            packet_id: "packet:recall:1".into(),
            query_summary: Some("Re-enter LifeGraph work".into()),
            rating,
            note: Some("Feedback from a turn.".into()),
            candidate_count: 4,
            connected_candidate_count: 1,
            missing_context_refs: vec!["life:goal:graph".into()],
            noisy_node_refs: vec![GraphRecordRef {
                id: "life:project:too-generic".into(),
                label: "Project".into(),
                datasource: Some("life-graph".into()),
            }],
            stale_node_refs: vec![GraphRecordRef {
                id: "life:open_loop:old".into(),
                label: "OpenLoop".into(),
                datasource: Some("life-graph".into()),
            }],
            evidence_packets: vec![],
        }
    }

    #[test]
    fn useful_recall_feedback_does_not_generate_patch() {
        let feedback = feedback_input(RetrievalFeedbackRating::Useful);
        assert!(recall_feedback_patch_proposal(&feedback).is_none());
    }

    #[test]
    fn disconnected_recall_feedback_generates_low_risk_system_patch() {
        let feedback = feedback_input(RetrievalFeedbackRating::Disconnected);
        let patch = recall_feedback_patch_proposal(&feedback)
            .expect("disconnected feedback should create patch proposal");

        assert_eq!(patch.patch_kind, PatchKind::SystemPatch);
        assert_eq!(patch.risk, PatchRisk::Low);
        assert_eq!(patch.evidence_packets[0].claim_ref.label, "Signal");
        assert!(patch.summary.contains("disconnected"));
    }

    #[test]
    fn overconfident_recall_feedback_generates_confirmation_gated_patch() {
        let feedback = feedback_input(RetrievalFeedbackRating::Overconfident);
        let patch = recall_feedback_patch_proposal(&feedback)
            .expect("overconfident feedback should create patch proposal");

        assert_eq!(patch.patch_kind, PatchKind::AttentionPatch);
        assert_eq!(patch.risk, PatchRisk::Medium);
        assert!(patch.rationale.contains("operator confirmation"));
    }

    #[test]
    fn raw_recall_fallback_returns_vector_hit_shape() {
        let cypher = raw_recall_fallback_cypher(&["Goal", "Habit"], 6);

        assert!(cypher.contains("MATCH (n)"));
        assert!(cypher.contains("'Goal'"));
        assert!(cypher.contains("'Habit'"));
        assert!(cypher.contains("RETURN n AS node, 0.25 AS similarity"));
        assert!(cypher.contains("LIMIT 6"));
    }

    #[test]
    fn commitments_approaching_cypher_returns_vector_hit_shape() {
        let cypher = commitments_approaching_cypher("2026-06-08T09:00:00Z");

        assert!(cypher.contains("MATCH (c:Commitment)"));
        assert!(cypher.contains("c.due_at <= '2026-06-08T09:00:00Z'"));
        assert!(cypher.contains("RETURN c AS node, 1.0 AS similarity"));
    }
}

/// Compute seconds elapsed since `observed_at` ISO 8601 string.
/// Returns 0 on parse failure (treat unknown age as fresh).
/// Call the ONNX sidecar's `/api/embeddings` endpoint and return `(vector, model_gen)`.
///
/// The sidecar address is read from `PHILOTIC_ONNX_SIDECAR_ADDR`
/// (default `http://127.0.0.1:11435`).
/// Returns an explicit error on dim mismatch — callers should surface this, not silently
/// continue with a wrong-dim vector.
async fn embed_text(text: &str) -> anyhow::Result<(Vec<f32>, String)> {
    let base = std::env::var("PHILOTIC_ONNX_SIDECAR_ADDR")
        .unwrap_or_else(|_| "http://127.0.0.1:11435".to_string());
    let url = format!("{base}/api/embeddings");

    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .post(&url)
        .json(&serde_json::json!({"prompt": text}))
        .send()
        .await
        .context("embed_text: HTTP request failed")?
        .json()
        .await
        .context("embed_text: failed to parse JSON response")?;

    if let Some(err) = resp.get("error").and_then(serde_json::Value::as_str) {
        anyhow::bail!("embed_text: sidecar error: {err}");
    }

    let vector: Vec<f32> = resp
        .get("embedding")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect()
        })
        .ok_or_else(|| anyhow::anyhow!("embed_text: response missing 'embedding' array"))?;

    let model_gen = resp
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    Ok((vector, model_gen))
}

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
