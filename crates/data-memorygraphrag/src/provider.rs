use anyhow::{Context, Result};
use async_trait::async_trait;
use data_memorygraphrag::LIFE_GRAPH_EMBEDDING_DIMS;
use data_memorygraphrag::cypher;
use data_memorygraphrag::projection;
use data_memorygraphrag::zoning;
use data_memorygraphrag::{
    AdjudicationStatus, ConflictHandoff, ContextPacket, EvidencePacket, GraphRecordRef,
    LifeCommitInput, LifeGraphToolRequest, LifeObserveInput, LifePatchProposalInput,
    LifeResolveInput, MemoryGraphRagRunner, PatchKind, PatchRisk, PolicyFilter, RankingWeights,
    ReliabilityBasis, RetrievalFeedbackInput, RetrievalFeedbackRating, RetrievalQuery,
    RetrievalStrategy, RunnerConfig, RunnerPlanTarget, SemanticSpace, SourceKind, SourceRef,
    SourceReliability, ValidationState,
};
use datasource::controller::{DatasourceProvider, DatasourceTask, ProviderOutput};
use neo4rs::{
    BoltList, BoltMap, BoltNode, BoltRelation, BoltType, BoltUnboundedRelation, ConfigBuilder,
    Graph, Row, query,
};
use serde_json::{Value, json};
use std::collections::HashSet;
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
        // Compile edges up-front so an unknown rel_type rejects the request
        // before any node write happens.
        let compiled_edges = cypher::compile_observe_edges(&input)
            .map_err(|e| anyhow::anyhow!("edge Cypher compilation failed: {e}"))?;

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
            .param("packet_id", compiled.packet_id.as_str())
            .param("observed_by", compiled.observed_by.as_str())
            .param(
                "observed_role",
                compiled.observed_role.as_deref().unwrap_or(""),
            );

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
            observed_by = %compiled.observed_by,
            "life.observe: proposed evidence node written to Memgraph"
        );

        // Living-cycle edge writes: MERGE'd idempotently against the freshly
        // written node. Missing targets create nothing and are reported per
        // edge; edge failures never fail the node write.
        let mut edge_reports = Vec::with_capacity(compiled_edges.len());
        for edge in &compiled_edges {
            let edge_query = query(&edge.query)
                .param("id", compiled.node_id.as_str())
                .param("target_id", edge.target_id.as_str())
                .param("created_at", now.as_str())
                .param("observation_id", compiled.observation_id.as_str())
                .param("observed_by", compiled.observed_by.as_str());
            let status = match graph.execute(edge_query).await {
                Ok(mut rows) => match rows.next().await {
                    Ok(Some(_)) => "written",
                    Ok(None) => {
                        warn!(
                            node_id = %compiled.node_id,
                            rel_type = %edge.rel_type,
                            target_id = %edge.target_id,
                            "life.observe edge target not found; edge skipped"
                        );
                        "target_missing"
                    }
                    Err(e) => {
                        warn!(
                            rel_type = %edge.rel_type,
                            target_id = %edge.target_id,
                            "life.observe edge result read failed: {e}"
                        );
                        "failed"
                    }
                },
                Err(e) => {
                    warn!(
                        rel_type = %edge.rel_type,
                        target_id = %edge.target_id,
                        "life.observe edge MERGE failed: {e}"
                    );
                    "failed"
                }
            };
            edge_reports.push(json!({
                "rel_type": edge.rel_type,
                "target_id": edge.target_id,
                "status": status,
            }));
        }

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
            "observed_by": compiled.observed_by,
            "observed_role": compiled.observed_role,
            "embed_status": embed_status,
            "edges": edge_reports,
        })))
    }

    async fn handle_recall(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        let query_val: RetrievalQuery = serde_json::from_value(task.parameters.clone())
            .context("failed to parse life.recall parameters as RetrievalQuery")?;
        let named_strategy = NamedRecallStrategy::from_task(task);
        if !named_strategy.agrees_with(&query_val.strategy) {
            warn!(
                named_strategy = named_strategy.as_str(),
                retrieval_strategy = ?query_val.strategy,
                "life.recall: RetrievalQuery.strategy disagrees with named_strategy; \
                 named_strategy drives dispatch"
            );
        }
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
        let weights = resolve_ranking_weights(&task.parameters, &query_val, named_strategy);
        let active_role = query_val.active_role.as_deref();
        let domain_edge_ids = self.domain_edge_node_ids(active_role, &all_hits).await;
        let mut scored = score_hits(
            all_hits,
            filters,
            &weights,
            &now,
            active_role,
            &domain_edge_ids,
        );

        let fallback_used = if scored.is_empty() {
            let fallback_labels = named_strategy.fallback_labels(&query_val);
            if fallback_labels.is_empty() {
                false
            } else {
                let limit = query_val.max_context_packets.max(1) * 3;
                let cypher = raw_recall_fallback_cypher(&fallback_labels, limit);
                let result = self.execute_cypher(&cypher).await?;
                let fallback_hits = projection::parse_vector_search_rows(&result);
                let fallback_domain_ids =
                    self.domain_edge_node_ids(active_role, &fallback_hits).await;
                scored = score_hits(
                    fallback_hits,
                    filters,
                    &weights,
                    &now,
                    active_role,
                    &fallback_domain_ids,
                );
                !scored.is_empty()
            }
        } else {
            false
        };
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Graph expansion (read side): one bounded living-cycle hop from the
        // ranked parents, batched into a single Cypher round trip. Expansion
        // failures never fail the recall — vector-only results still return.
        let mut candidates: Vec<projection::ScoredHit> =
            scored.into_iter().map(Into::into).collect();
        let expansion_policy = &query_val.expansion_policy;
        let mut expansion_count = 0usize;
        if expansion_policy.max_hops >= 1
            && expansion_policy.max_nodes > 0
            && !candidates.is_empty()
        {
            let expansion_cypher = {
                let rel_types =
                    projection::expansion_rel_types(&expansion_policy.allowed_edge_types);
                let seeds: Vec<&str> = candidates
                    .iter()
                    .take(query_val.max_context_packets.max(1))
                    .map(|c| c.hit.node_id())
                    .filter(|id| !id.is_empty())
                    .collect();
                if rel_types.is_empty() || seeds.is_empty() {
                    None
                } else {
                    Some(projection::expansion_cypher(
                        &seeds,
                        &rel_types,
                        expansion_policy.max_nodes,
                    ))
                }
            };
            if let Some(cypher) = expansion_cypher {
                match self.execute_cypher(&cypher).await {
                    Ok(result) => {
                        let expansion_hits = projection::parse_expansion_rows(&result);
                        let folded = projection::fold_expansion_hits(
                            &candidates,
                            expansion_hits,
                            filters,
                            projection::EXPANSION_SCORE_DECAY,
                            expansion_policy.max_nodes,
                        );
                        expansion_count = folded.len();
                        candidates.extend(folded);
                        candidates.sort_by(|a, b| {
                            b.score
                                .partial_cmp(&a.score)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        });
                    }
                    Err(e) => {
                        warn!(
                            query_id = %query_val.query_id,
                            "life.recall: living-cycle expansion failed; \
                             returning vector-only results: {e}"
                        );
                    }
                }
            }
        }

        let context_id = format!("ctx:{}", query_val.query_id);
        let token_budget = query_val.max_context_packets * 200;
        let packet = projection::project_context_packet(
            &context_id,
            &query_val.query_id,
            query_val.strategy.clone(),
            candidates,
            Vec::new(),
            token_budget,
            &now_iso,
        );

        info!(
            query_id = %query_val.query_id,
            result_count = packet.ranked_packets.len(),
            expansion_count,
            fallback_used,
            "life.recall: context packet projected"
        );

        let packet_json =
            serde_json::to_value(&packet).context("failed to serialize RetrievalContextPacket")?;
        let cross_agent_packet = ContextPacket::from_lifegraph_retrieval(
            &packet,
            format!("LifeGraph recall for {}", query_val.query_text),
            query_val.active_role.clone(),
        );
        let cross_agent_packet_json = serde_json::to_value(&cross_agent_packet)
            .context("failed to serialize cross-agent ContextPacket")?;

        Ok(ProviderOutput::ResultSet(json!({
            "status": "ok",
            "named_strategy": named_strategy.as_str(),
            "fallback_used": fallback_used,
            "context_packet": packet_json,
            "cross_agent_context_packet": cross_agent_packet_json,
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

    /// Node ids among `hits` tied to the caller's domain by a living-cycle
    /// edge to the V005 domain Role node.
    ///
    /// Best-effort bias signal: any failure (unknown slug, Cypher error)
    /// degrades to an empty set with a warning — ranking then falls back to
    /// the property-only provenance check. Never filters anything.
    async fn domain_edge_node_ids(
        &self,
        active_role: Option<&str>,
        hits: &[projection::VectorHit],
    ) -> HashSet<String> {
        let Some(slug) = active_role else {
            return HashSet::new();
        };
        let Some(role_node_id) = zoning::role_node_id_for_domain(slug) else {
            warn!(
                active_role = slug,
                "life.recall: active_role is not a known V005 domain slug; \
                 living-cycle role bonus skipped"
            );
            return HashSet::new();
        };
        let node_ids: Vec<&str> = hits
            .iter()
            .map(|hit| hit.node_id())
            .filter(|id| !id.is_empty())
            .collect();
        if node_ids.is_empty() {
            return HashSet::new();
        }
        let cypher = domain_edge_nodes_cypher(role_node_id, &node_ids);
        match self.execute_cypher(&cypher).await {
            Ok(result) => parse_node_id_rows(&result),
            Err(e) => {
                warn!(
                    active_role = slug,
                    "life.recall: living-cycle domain edge lookup failed; \
                     role bonus degrades to provenance-only: {e}"
                );
                HashSet::new()
            }
        }
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
    /// Strict enum validation of a wire strategy name. `None` for anything
    /// outside the documented vocabulary.
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "semantic_pivot" => Some(Self::SemanticPivot),
            "open_loops_by_context" => Some(Self::OpenLoopsByContext),
            "goals_and_next_actions" => Some(Self::GoalsAndNextActions),
            "commitments_approaching" => Some(Self::CommitmentsApproaching),
            "re_entry_context" => Some(Self::ReEntryContext),
            "cross_domain_entanglement" => Some(Self::CrossDomainEntanglement),
            _ => None,
        }
    }

    fn from_task(task: &DatasourceTask) -> Self {
        // Explicit strategy keys are validated strictly (wire compat: unknown
        // values still degrade to semantic_pivot, but loudly instead of
        // silently).
        if let Some(raw) = task
            .parameters
            .get("named_strategy")
            .or_else(|| task.parameters.get("strategy_name"))
            .and_then(Value::as_str)
        {
            return match Self::parse(raw) {
                Some(strategy) => strategy,
                None => {
                    warn!(
                        named_strategy = raw,
                        "life.recall: unknown named_strategy; expected one of \
                         semantic_pivot, open_loops_by_context, goals_and_next_actions, \
                         commitments_approaching, re_entry_context, \
                         cross_domain_entanglement; falling back to semantic_pivot"
                    );
                    Self::SemanticPivot
                }
            };
        }

        // operator_intent is a soft hint, not a strategy field — it may carry
        // free text, so an unrecognized value is not warned about.
        task.parameters
            .get("operator_intent")
            .and_then(Value::as_str)
            .and_then(Self::parse)
            .unwrap_or(Self::SemanticPivot)
    }

    /// Whether `RetrievalQuery.strategy` is consistent with this named
    /// strategy. The named recipes are memory-aware graph-rank plans, so any
    /// explicit non-default `strategy` alongside them is a caller
    /// inconsistency worth a warning (dispatch still follows named_strategy).
    fn agrees_with(self, strategy: &RetrievalStrategy) -> bool {
        match self {
            // SemanticPivot is both the explicit strategy and the fallback
            // when no named_strategy is given — never warn for it.
            Self::SemanticPivot => true,
            _ => matches!(strategy, RetrievalStrategy::MemoryAwareGraphRank),
        }
    }

    /// Server-side default ranking weight profile used when the caller omits
    /// `ranking_weights`. Base weights sum to 1.0; `role_relevance` rides on
    /// top as the soft-zoning bonus (score clamps at 1.0).
    fn default_ranking_weights(self) -> RankingWeights {
        match self {
            // Re-entry cares about what happened *recently*.
            Self::ReEntryContext => RankingWeights {
                semantic_similarity: 0.35,
                graph_specificity: 0.15,
                recency: 0.30,
                confirmation: 0.15,
                active_commitment: 0.05,
                ..RankingWeights::default()
            },
            // Open loops care about what is still *actively committed*.
            Self::OpenLoopsByContext => RankingWeights {
                semantic_similarity: 0.35,
                graph_specificity: 0.15,
                recency: 0.10,
                confirmation: 0.10,
                active_commitment: 0.30,
                ..RankingWeights::default()
            },
            _ => RankingWeights::default(),
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

/// Ranking weights for a recall request: the caller's explicit
/// `ranking_weights` win; when omitted, the named strategy's server-side
/// default profile applies.
fn resolve_ranking_weights(
    parameters: &Value,
    query: &RetrievalQuery,
    named_strategy: NamedRecallStrategy,
) -> RankingWeights {
    let caller_supplied = parameters
        .get("ranking_weights")
        .is_some_and(|value| !value.is_null());
    if caller_supplied {
        query.ranking_weights.clone()
    } else {
        named_strategy.default_ranking_weights()
    }
}

/// Score policy-filtered hits, applying the soft-zoning role bonus.
///
/// A hit earns the `role_relevance` bonus when the caller has an
/// `active_role` domain and the hit either has a living-cycle edge to that
/// domain's Role node (`domain_edge_ids`, provider-fetched) or its
/// provenance/zoning properties tie it to the domain. The bonus never
/// filters: unmatched hits keep their full base score.
fn score_hits(
    hits: Vec<projection::VectorHit>,
    filters: &[PolicyFilter],
    weights: &RankingWeights,
    now: &chrono::DateTime<chrono::Utc>,
    active_role: Option<&str>,
    domain_edge_ids: &HashSet<String>,
) -> Vec<(projection::VectorHit, f32, Vec<PolicyFilter>)> {
    let (surviving, _drop_log) = projection::apply_policy_filters(hits, filters);
    surviving
        .into_iter()
        .map(|hit| {
            let age_secs = compute_age_secs(hit.prop_str("observed_at"), now);
            let role_matched = active_role.is_some_and(|slug| {
                domain_edge_ids.contains(hit.node_id())
                    || projection::hit_matches_domain(&hit, slug)
            });
            let score = projection::ranking_score(&hit, weights, age_secs, role_matched);
            (hit, score, Vec::new())
        })
        .collect()
}

fn escape_cypher_single_quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Cypher for the living-cycle domain membership check: which of `node_ids`
/// have a living-cycle edge (either direction) to the domain's Role node.
fn domain_edge_nodes_cypher(role_node_id: &str, node_ids: &[&str]) -> String {
    let ids = node_ids
        .iter()
        .map(|id| format!("'{}'", escape_cypher_single_quoted(id)))
        .collect::<Vec<_>>()
        .join(", ");
    let rel_types = cypher::LIVING_CYCLE_REL_TYPES
        .iter()
        .map(|rel| format!("'{rel}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "MATCH (n)-[r]-(role:Role {{id: '{role_id}'}}) \
         WHERE type(r) IN [{rel_types}] AND n.id IN [{ids}] \
         RETURN DISTINCT n.id AS node_id",
        role_id = escape_cypher_single_quoted(role_node_id),
        rel_types = rel_types,
        ids = ids,
    )
}

/// Parse `RETURN ... AS node_id` rows into a set of node ids.
fn parse_node_id_rows(result: &Value) -> HashSet<String> {
    result
        .get("rows")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("node_id").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
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

    #[test]
    fn named_strategy_parse_validates_against_real_enum() {
        assert_eq!(
            NamedRecallStrategy::parse("open_loops_by_context"),
            Some(NamedRecallStrategy::OpenLoopsByContext)
        );
        assert_eq!(
            NamedRecallStrategy::parse("semantic_pivot"),
            Some(NamedRecallStrategy::SemanticPivot)
        );
        assert_eq!(NamedRecallStrategy::parse("surprise_me"), None);
        assert_eq!(NamedRecallStrategy::parse(""), None);
    }

    #[test]
    fn free_text_operator_intent_does_not_dispatch_a_named_strategy() {
        // operator_intent is a soft hint: free text falls through quietly.
        let task = task_with_params(json!({ "operator_intent": "attention planning" }));
        assert_eq!(
            NamedRecallStrategy::from_task(&task),
            NamedRecallStrategy::SemanticPivot
        );
    }

    #[test]
    fn named_strategy_agreement_with_retrieval_strategy() {
        // The named recipes are memory-aware graph-rank plans.
        assert!(
            NamedRecallStrategy::OpenLoopsByContext
                .agrees_with(&RetrievalStrategy::MemoryAwareGraphRank)
        );
        assert!(
            !NamedRecallStrategy::OpenLoopsByContext.agrees_with(&RetrievalStrategy::SemanticPivot)
        );
        assert!(
            !NamedRecallStrategy::ReEntryContext.agrees_with(&RetrievalStrategy::VectorThenExpand)
        );
        // SemanticPivot is also the no-named-strategy fallback: never warns.
        assert!(NamedRecallStrategy::SemanticPivot.agrees_with(&RetrievalStrategy::SemanticPivot));
        assert!(
            NamedRecallStrategy::SemanticPivot
                .agrees_with(&RetrievalStrategy::MemoryAwareGraphRank)
        );
    }

    #[test]
    fn re_entry_context_default_weights_favor_recency() {
        let weights = NamedRecallStrategy::ReEntryContext.default_ranking_weights();
        let base = RankingWeights::default();
        assert!(weights.recency > base.recency);
        assert!(weights.recency > weights.active_commitment);
        let sum = weights.semantic_similarity
            + weights.graph_specificity
            + weights.recency
            + weights.confirmation
            + weights.active_commitment;
        assert!((sum - 1.0).abs() < 0.001, "base weights should sum to 1.0");
        assert!((weights.role_relevance - base.role_relevance).abs() < f32::EPSILON);
    }

    #[test]
    fn open_loops_default_weights_favor_active_commitment() {
        let weights = NamedRecallStrategy::OpenLoopsByContext.default_ranking_weights();
        let base = RankingWeights::default();
        assert!(weights.active_commitment > base.active_commitment);
        assert!(weights.active_commitment > weights.recency);
        let sum = weights.semantic_similarity
            + weights.graph_specificity
            + weights.recency
            + weights.confirmation
            + weights.active_commitment;
        assert!((sum - 1.0).abs() < 0.001, "base weights should sum to 1.0");
        // Other strategies keep the contract default.
        assert_eq!(
            NamedRecallStrategy::SemanticPivot.default_ranking_weights(),
            RankingWeights::default()
        );
        assert_eq!(
            NamedRecallStrategy::GoalsAndNextActions.default_ranking_weights(),
            RankingWeights::default()
        );
    }

    #[test]
    fn resolve_ranking_weights_prefers_caller_supplied_weights() {
        let params = json!({
            "named_strategy": "re_entry_context",
            "ranking_weights": {
                "semantic_similarity": 0.9,
                "graph_specificity": 0.025,
                "recency": 0.025,
                "confirmation": 0.025,
                "active_commitment": 0.025
            }
        });
        let query: RetrievalQuery = serde_json::from_value(json!({
            "query_id": "q:explicit",
            "query_text": "explicit weights",
            "ranking_weights": params["ranking_weights"].clone()
        }))
        .unwrap();
        let weights = resolve_ranking_weights(&params, &query, NamedRecallStrategy::ReEntryContext);
        assert!((weights.semantic_similarity - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn resolve_ranking_weights_uses_strategy_defaults_when_omitted() {
        // The auto-recall lane omits ranking_weights entirely.
        let params = json!({
            "query_id": "q:auto",
            "query_text": "auto recall",
            "named_strategy": "re_entry_context",
            "active_role": "chief_of_staff"
        });
        let query: RetrievalQuery = serde_json::from_value(params.clone()).unwrap();
        let weights = resolve_ranking_weights(&params, &query, NamedRecallStrategy::ReEntryContext);
        assert_eq!(
            weights,
            NamedRecallStrategy::ReEntryContext.default_ranking_weights()
        );
        assert_ne!(weights, RankingWeights::default());
    }

    fn domain_hit(id: &str, observed_by: &str) -> projection::VectorHit {
        projection::VectorHit {
            bolt_id: 1,
            label: "OpenLoop".to_string(),
            properties: json!({
                "id": id,
                "title": "loop",
                "confidence": 0.7,
                "validation_state": "proposed",
                "status": "open",
                "observed_by": observed_by,
                "observed_at": "2026-07-01T10:00:00Z"
            }),
            similarity: 0.8,
        }
    }

    #[test]
    fn score_hits_applies_role_bonus_without_filtering_cross_domain_hits() {
        let now = chrono::Utc::now();
        let weights = RankingWeights::default();
        let hits = vec![
            domain_hit("l:ol:mine", "agent-beacon"),
            domain_hit("l:ol:other", "agent-astrid"),
            domain_hit("l:ol:edge-only", "agent:unknown"),
        ];
        let mut edge_ids = HashSet::new();
        edge_ids.insert("l:ol:edge-only".to_string());

        let scored = score_hits(hits, &[], &weights, &now, Some("chief_of_staff"), &edge_ids);

        // Soft boundaries: every hit survives, none are filtered.
        assert_eq!(scored.len(), 3, "role bias must never filter hits");

        let score_of = |id: &str| {
            scored
                .iter()
                .find(|(hit, _, _)| hit.node_id() == id)
                .map(|(_, score, _)| *score)
                .unwrap()
        };
        // Provenance match and living-cycle edge match both earn the bonus;
        // the cross-domain hit keeps its (lower) base score.
        assert!(score_of("l:ol:mine") > score_of("l:ol:other"));
        assert!(score_of("l:ol:edge-only") > score_of("l:ol:other"));
        assert!(
            (score_of("l:ol:mine") - score_of("l:ol:other") - weights.role_relevance).abs() < 0.001
        );
    }

    #[test]
    fn score_hits_without_active_role_applies_no_bonus() {
        let now = chrono::Utc::now();
        let weights = RankingWeights::default();
        let hits = vec![
            domain_hit("l:ol:mine", "agent-beacon"),
            domain_hit("l:ol:other", "agent-astrid"),
        ];
        let scored = score_hits(hits, &[], &weights, &now, None, &HashSet::new());
        assert_eq!(scored.len(), 2);
        assert!(
            (scored[0].1 - scored[1].1).abs() < f32::EPSILON,
            "without an active_role the domain bias must be a no-op"
        );
    }

    #[test]
    fn domain_edge_nodes_cypher_matches_living_cycle_edges_only() {
        let cypher =
            domain_edge_nodes_cypher("life:role:chief-of-staff", &["l:ol:a", "l:ol:b'quote"]);
        assert!(cypher.contains("(role:Role {id: 'life:role:chief-of-staff'})"));
        assert!(cypher.contains("type(r) IN ['OWNS', 'SHAPES', 'SETS', 'SPAWNS', 'RELATES_TO']"));
        assert!(cypher.contains("n.id IN ['l:ol:a', 'l:ol:b\\'quote']"));
        assert!(cypher.contains("RETURN DISTINCT n.id AS node_id"));
    }

    #[test]
    fn parse_node_id_rows_collects_ids() {
        let result = json!({
            "rows": [
                { "node_id": "l:ol:a" },
                { "node_id": "l:ol:b" },
                { "unrelated": true }
            ]
        });
        let ids = parse_node_id_rows(&result);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("l:ol:a"));
        assert!(ids.contains("l:ol:b"));
        assert!(parse_node_id_rows(&json!({})).is_empty());
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
