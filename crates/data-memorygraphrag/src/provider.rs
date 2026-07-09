use anyhow::{Context, Result};
use async_trait::async_trait;
use data_memorygraphrag::LIFE_GRAPH_EMBEDDING_DIMS;
use data_memorygraphrag::cypher;
use data_memorygraphrag::entanglement;
use data_memorygraphrag::projection;
use data_memorygraphrag::zoning;
use data_memorygraphrag::{
    AdjudicationStatus, ConflictHandoff, ContextPacket, EvidencePacket, FeedbackEdgeSpec,
    GraphRecordRef, LifeCommitInput, LifeGraphToolRequest, LifeObserveInput, LifePatchApplyInput,
    LifePatchListInput, LifePatchProposalInput, LifeResolveInput, MemoryGraphRagRunner,
    PatchApplyDecision, PatchGate, PatchKind, PatchRisk, PolicyFilter, RankingWeights,
    ReliabilityBasis, RetrievalFeedbackInput, RetrievalFeedbackRating, RetrievalQuery,
    RetrievalStrategy, RunnerConfig, RunnerPlanTarget, SemanticSpace, SourceKind, SourceRef,
    SourceReliability, ValidationState, feedback_edge_specs,
};
use datasource::controller::{DatasourceProvider, DatasourceTask, ProviderOutput};
use neo4rs::{
    BoltList, BoltMap, BoltNode, BoltRelation, BoltType, BoltUnboundedRelation, ConfigBuilder,
    Graph, Row, query,
};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{info, warn};

/// Default minimum cosine similarity gate for the named recall strategies.
///
/// Live calibration (2026-07-07, post data-hygiene, 53 live fully-embedded
/// nodes): the realistic conversational probe "open loops about errands and
/// daily tasks" returned top-3 similarities 0.505 / 0.320 / 0.192. Real hits
/// live in the 0.19-0.51 band, so the previous hardcoded gates (0.4 for
/// OpenLoop/Event, 0.35 for Goal) excluded most of them and forced the raw
/// recency fallback (`fallback_used=true`) on every production recall —
/// returning rows with NO semantic relevance instead of low-similarity
/// vector hits. 0.18 sits just under the observed real-hit floor while
/// still cutting unrelated noise.
const DEFAULT_RECALL_MIN_SIMILARITY: f32 = 0.18;

/// Runner-side env override for the recall similarity gate.
/// Parsed once per process; clamped to `[0.0, 0.9]`; invalid values fall
/// back to [`DEFAULT_RECALL_MIN_SIMILARITY`] with a warning.
const RECALL_MIN_SIMILARITY_ENV: &str = "PHILOTIC_LIFE_RECALL_MIN_SIMILARITY";

/// Fallback top-up hits are rescaled so their best score lands at this
/// fraction of the weakest vector hit's score: recency-scan rows are always
/// ranked strictly below every semantically-matched row while preserving
/// their relative order among themselves.
const FALLBACK_TOPUP_DAMP: f32 = 0.9;

/// Pure parse of the similarity-gate override (testable without env).
fn parse_recall_min_similarity(raw: Option<&str>) -> f32 {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return DEFAULT_RECALL_MIN_SIMILARITY;
    };
    match raw.parse::<f32>() {
        Ok(v) if v.is_finite() => v.clamp(0.0, 0.9),
        _ => {
            warn!(
                value = raw,
                default = DEFAULT_RECALL_MIN_SIMILARITY,
                "invalid {RECALL_MIN_SIMILARITY_ENV}; using default"
            );
            DEFAULT_RECALL_MIN_SIMILARITY
        }
    }
}

/// Env-reading variant, uncached (used by the cached getter and by tests).
fn recall_min_similarity_from_env() -> f32 {
    parse_recall_min_similarity(std::env::var(RECALL_MIN_SIMILARITY_ENV).ok().as_deref())
}

/// The effective recall similarity gate: env override parsed once, cached
/// for the life of the runner process.
fn recall_min_similarity() -> f32 {
    static CACHE: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *CACHE.get_or_init(recall_min_similarity_from_env)
}

/// How the raw recency-scan fallback participated in a recall response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FallbackUsage {
    /// Vector search alone filled the packet (or nothing was found at all).
    None,
    /// Vector search returned some hits but fewer than
    /// `max_context_packets`; the remainder was topped up from the raw
    /// fallback, ranked below every vector hit.
    ToppedUp,
    /// Vector search returned zero hits; the packet is entirely raw
    /// fallback rows.
    Full,
}

impl FallbackUsage {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "false",
            Self::ToppedUp => "topped_up",
            Self::Full => "full_fallback",
        }
    }
}

type ScoredTuple = (projection::VectorHit, f32, Vec<PolicyFilter>);

fn sort_hits_desc(hits: &mut [projection::ScoredHit]) {
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Blend vector-search hits with raw recency-fallback hits.
///
/// - Enough vector hits (>= `max_context_packets`) or an empty fallback:
///   vector hits only, [`FallbackUsage::None`].
/// - Some vector hits but fewer than `max_context_packets`: top up with the
///   best fallback rows (deduped by node id), marked `fallback_origin` and
///   rescaled strictly below the weakest vector hit — semantic relevance
///   always outranks recency ([`FallbackUsage::ToppedUp`]).
/// - Zero vector hits: the packet is entirely fallback rows, all marked
///   `fallback_origin` ([`FallbackUsage::Full`]); if the fallback is also
///   empty this is [`FallbackUsage::None`].
fn blend_with_fallback(
    vector_scored: Vec<ScoredTuple>,
    fallback_scored: Vec<ScoredTuple>,
    max_context_packets: usize,
) -> (Vec<projection::ScoredHit>, FallbackUsage) {
    let mark_fallback = |tuple: ScoredTuple| {
        let mut scored: projection::ScoredHit = tuple.into();
        scored.fallback_origin = true;
        scored
    };

    let mut vector: Vec<projection::ScoredHit> =
        vector_scored.into_iter().map(Into::into).collect();
    sort_hits_desc(&mut vector);

    if vector.is_empty() {
        if fallback_scored.is_empty() {
            return (Vec::new(), FallbackUsage::None);
        }
        let mut full: Vec<projection::ScoredHit> =
            fallback_scored.into_iter().map(mark_fallback).collect();
        sort_hits_desc(&mut full);
        return (full, FallbackUsage::Full);
    }

    let needed = max_context_packets.saturating_sub(vector.len());
    if needed == 0 || fallback_scored.is_empty() {
        return (vector, FallbackUsage::None);
    }

    let seen: HashSet<&str> = vector.iter().map(|s| s.hit.node_id()).collect();
    let mut top_up: Vec<projection::ScoredHit> = fallback_scored
        .iter()
        .filter(|(hit, _, _)| !seen.contains(hit.node_id()))
        .cloned()
        .map(mark_fallback)
        .collect();
    sort_hits_desc(&mut top_up);
    top_up.truncate(needed);
    if top_up.is_empty() {
        return (vector, FallbackUsage::None);
    }

    // Rescale so the best top-up row sits at FALLBACK_TOPUP_DAMP of the
    // weakest vector hit: strictly below every semantic hit, relative
    // order among fallback rows preserved.
    let floor = vector.last().map(|s| s.score).unwrap_or(0.0);
    let max_top_up = top_up.first().map(|s| s.score).unwrap_or(0.0);
    let scale = if max_top_up > 0.0 {
        (floor * FALLBACK_TOPUP_DAMP) / max_top_up
    } else {
        0.0
    };
    for scored in &mut top_up {
        scored.score = (scored.score * scale).clamp(0.0, 1.0);
    }

    vector.extend(top_up);
    (vector, FallbackUsage::ToppedUp)
}

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

// ── Autonomy gate (Autopoiesis Slice A2, lane graph.bridge_edges) ────────────

/// The hotel's answer to a `ConsumeAutonomyAction` consult.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutonomyDecision {
    pub allowed: bool,
    pub posture: Option<String>,
    pub audit_id: Option<String>,
    pub reason: Option<String>,
}

impl AutonomyDecision {
    fn from_response_data(data: &Value) -> Self {
        let get = |key: &str| {
            data.get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|s| !s.is_empty())
        };
        Self {
            allowed: data
                .get("allowed")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            posture: get("posture"),
            audit_id: get("audit_id"),
            reason: get("reason"),
        }
    }
}

/// How the runner reaches the hotel's autonomy grant machinery. The runner is
/// a guest — it never touches `GraphDomain` directly; it asks over IPC and
/// the hotel owns the decision (kill switch, posture, budget, audit).
#[async_trait]
pub trait AutonomyGate: Send + Sync {
    async fn consume_action(
        &self,
        lane: &str,
        action_summary: &str,
        evidence: &str,
        reversal_hint: &str,
    ) -> Result<AutonomyDecision>;

    /// Report an operator-reviewed outcome (`"confirmed_good"` |
    /// `"reversed"`) against a hotel audit id.
    async fn record_outcome(&self, audit_id: &str, outcome: &str) -> Result<Value>;
}

/// Production [`AutonomyGate`]: a short-lived hotel IPC connection per
/// consult. Registers under the side role `"autonomy-consult"` — nothing
/// routes tasks to that role, so the ephemeral connection can never steal
/// inbox work from the runner's main controller connection.
pub struct HotelAutonomyGate;

impl HotelAutonomyGate {
    async fn hotel_request(req: philotic_client::IpcRequest) -> Result<Value> {
        use philotic_client::{GuestIdentity, IpcResponse, PhiloticClient};

        let runner_id = std::env::var("PHILOTIC_LIFE_GRAPH_RUNNER_ID")
            .or_else(|_| std::env::var("PHILOTIC_GRAPH_RUNNER_ID"))
            .unwrap_or_else(|_| "life-graph-runner".to_string());
        let identity = GuestIdentity {
            guest_id: format!("{runner_id}-autonomy"),
            role: "autonomy-consult".to_string(),
            supported_tools: Vec::new(),
        };
        let mut client = PhiloticClient::connect(identity)
            .await
            .context("autonomy consult: hotel IPC connect failed")?;
        match client.send_request(req).await? {
            IpcResponse::Standard {
                ok: true,
                data: Some(data),
                ..
            } => Ok(data),
            IpcResponse::Standard {
                ok: false,
                code,
                message,
                ..
            } => anyhow::bail!("autonomy consult refused by hotel: {code}: {message}"),
            other => anyhow::bail!("autonomy consult: unexpected hotel response: {other:?}"),
        }
    }
}

#[async_trait]
impl AutonomyGate for HotelAutonomyGate {
    async fn consume_action(
        &self,
        lane: &str,
        action_summary: &str,
        evidence: &str,
        reversal_hint: &str,
    ) -> Result<AutonomyDecision> {
        let data = Self::hotel_request(philotic_client::IpcRequest::ConsumeAutonomyAction {
            lane: lane.to_string(),
            action_summary: action_summary.to_string(),
            evidence: evidence.to_string(),
            reversal_hint: reversal_hint.to_string(),
        })
        .await?;
        Ok(AutonomyDecision::from_response_data(&data))
    }

    async fn record_outcome(&self, audit_id: &str, outcome: &str) -> Result<Value> {
        Self::hotel_request(philotic_client::IpcRequest::RecordAutonomyOutcome {
            audit_id: audit_id.to_string(),
            outcome: outcome.to_string(),
        })
        .await
    }
}

/// What the runner does with the derived edge specs, by hotel posture.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BridgeAction {
    /// AutoWithAudit: MERGE the edges now; the audit record is the ledger.
    ApplyNow { audit_id: String },
    /// ConfirmFirst: do NOT write — file the patch `awaiting_confirmation`
    /// with the ready-to-apply specs embedded; `life.patch.apply` executes
    /// them on operator confirm.
    AwaitConfirmation { audit_id: String },
    /// ProposalOnly / lane disabled / frozen / budget exhausted /
    /// consult unavailable: prose-only patch, exactly as before Slice A2.
    ProseOnly { reason: String },
}

/// Pure mapping from the hotel's decision to the runner's behavior.
/// Fail-safe: anything malformed (missing audit id, unknown posture)
/// degrades to prose-only — the loop never writes without a granted posture
/// AND an audit anchor.
fn plan_bridge_action(decision: &AutonomyDecision) -> BridgeAction {
    let posture = decision.posture.as_deref().unwrap_or("");
    match (decision.allowed, posture, decision.audit_id.as_ref()) {
        (true, "auto_with_audit", Some(audit_id)) => BridgeAction::ApplyNow {
            audit_id: audit_id.clone(),
        },
        (false, "confirm_first", Some(audit_id)) => BridgeAction::AwaitConfirmation {
            audit_id: audit_id.clone(),
        },
        _ => BridgeAction::ProseOnly {
            reason: decision
                .reason
                .clone()
                .or_else(|| decision.posture.clone().map(|p| format!("posture_{p}")))
                .unwrap_or_else(|| "no_grant".to_string()),
        },
    }
}

pub struct LifeGraphProvider {
    config: MemgraphConfig,
    runner: MemoryGraphRagRunner,
    autonomy: Arc<dyn AutonomyGate>,
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
            autonomy: Arc::new(HotelAutonomyGate),
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
            "life.patch.apply" => self.handle_patch_apply(task).await,
            "life.patch.list" => self.handle_patch_list(task).await,
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
            )
            // Sentinels ('' / -1.0) become null in the compiled CASE clauses.
            .param(
                "origin_engram_id",
                compiled.origin_engram_id.as_deref().unwrap_or(""),
            )
            .param("origin_trust", compiled.origin_trust.unwrap_or(-1.0));

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
            "origin_engram_id": compiled.origin_engram_id,
            "origin_trust": compiled.origin_trust,
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
        let min_similarity = recall_min_similarity();
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
                    min_similarity,
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
                    min_similarity,
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
                        min_similarity,
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
                    min_similarity,
                    &embedding,
                )
                .await?;
                self.extend_vector_hits(
                    &mut all_hits,
                    SemanticSpace::GoalSystemSemantic,
                    &["Goal"],
                    5,
                    min_similarity,
                    &embedding,
                )
                .await?;
            }
            NamedRecallStrategy::CrossDomainEntanglement => {
                // Dual-similarity intersection + living-cycle bridge discovery
                // — a dedicated pipeline, not the shared concat/score path.
                return self
                    .handle_cross_domain_recall(task, &query_val, &embedding, &now_iso)
                    .await;
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
        let vector_scored = score_hits(
            all_hits,
            filters,
            &weights,
            &now,
            active_role,
            &domain_edge_ids,
        );

        // Blend, don't cliff: when vector search yields fewer hits than
        // max_context_packets, top up from the raw recency fallback (ranked
        // below every vector hit, marked fallback_origin) instead of an
        // all-or-nothing switch.
        let max_packets = query_val.max_context_packets.max(1);
        let fallback_scored = if vector_scored.len() < max_packets {
            let fallback_labels = named_strategy.fallback_labels(&query_val);
            if fallback_labels.is_empty() {
                Vec::new()
            } else {
                let limit = max_packets * 3;
                let cypher = raw_recall_fallback_cypher(&fallback_labels, limit);
                let result = self.execute_cypher(&cypher).await?;
                let fallback_hits = projection::parse_vector_search_rows(&result);
                let fallback_domain_ids =
                    self.domain_edge_node_ids(active_role, &fallback_hits).await;
                score_hits(
                    fallback_hits,
                    filters,
                    &weights,
                    &now,
                    active_role,
                    &fallback_domain_ids,
                )
            }
        } else {
            Vec::new()
        };
        let (mut candidates, fallback_usage) =
            blend_with_fallback(vector_scored, fallback_scored, max_packets);

        // Graph expansion (read side): one bounded living-cycle hop from the
        // ranked parents, batched into a single Cypher round trip. Expansion
        // failures never fail the recall — vector-only results still return.
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
            fallback_used = fallback_usage.as_str(),
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
            "fallback_used": fallback_usage.as_str(),
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
                    .param("packet_id", compiled.packet_id.as_str())
                    .param("loop_status", compiled.loop_status.as_str())
                    .param("resolution_note", compiled.resolution_note.as_str()),
            )
            .await?;
        let first_row = rows.next().await?;
        let node_id = first_row
            .as_ref()
            .and_then(|r| r.get::<String>("id").ok())
            .unwrap_or_else(|| compiled.node_id.clone());
        let loop_status = first_row
            .as_ref()
            .and_then(|r| r.get::<String>("status").ok());

        Ok(ProviderOutput::ResultSet(json!({
            "status": "committed",
            "node_id": node_id,
            "label": compiled.label,
            "packet_id": compiled.packet_id,
            "validation_state": "confirmed",
            "loop_status": loop_status,
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

        // ── Feedback-to-action (Autopoiesis Slice A2) ─────────────────────
        // SafeAutoUpdate feedback whose remedy is structural and unambiguous
        // (disconnected/missing with an anchor + candidate node ids) is
        // routed through the hotel's graph.bridge_edges autonomy lane. The
        // patch node is always written — it is the audit trail's evidence —
        // but its status and embedded edge specs depend on the lane posture.
        let mut generated_patch = recall_feedback_patch_proposal(&input);
        let mut bridge_summary: Option<Value> = None;
        let mut patch_status = cypher::PATCH_STATUS_PROPOSED;
        if let Some(patch) = generated_patch.as_mut() {
            let specs = feedback_edge_specs(&input, &now);
            if patch.risk.gate() == PatchGate::SafeAutoUpdate && !specs.is_empty() {
                let action = self.consult_bridge_lane(&input, &specs).await;
                match action {
                    BridgeAction::ApplyNow { audit_id } => {
                        let (edges_written, missing_targets) =
                            Self::execute_bridge_edges(&graph, &specs).await?;
                        patch.edge_specs = specs;
                        patch.autonomy_audit_id = Some(audit_id.clone());
                        patch_status = cypher::PATCH_STATUS_APPLIED;
                        bridge_summary = Some(json!({
                            "action": "applied",
                            "audit_id": audit_id,
                            "edges_written": edges_written,
                            "missing_targets": missing_targets,
                        }));
                    }
                    BridgeAction::AwaitConfirmation { audit_id } => {
                        patch.edge_specs = specs;
                        patch.autonomy_audit_id = Some(audit_id.clone());
                        patch_status = cypher::PATCH_STATUS_AWAITING_CONFIRMATION;
                        bridge_summary = Some(json!({
                            "action": "awaiting_confirmation",
                            "audit_id": audit_id,
                            "edge_spec_count": patch.edge_specs.len(),
                        }));
                    }
                    BridgeAction::ProseOnly { reason } => {
                        bridge_summary = Some(json!({
                            "action": "prose_only",
                            "reason": reason,
                        }));
                    }
                }
            }
        }

        let generated_patch_summary = if let Some(patch) = &generated_patch {
            let compiled_patch =
                cypher::compile_patch_proposal_with_status(patch, &now, patch_status).map_err(
                    |e| {
                        anyhow::anyhow!("life.recall.feedback patch Cypher compilation failed: {e}")
                    },
                )?;
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
                "bridge": bridge_summary,
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

    /// Ask the hotel's `graph.bridge_edges` lane for permission to bridge.
    /// Any consult failure degrades to prose-only with a reason — the loop
    /// never writes on an unanswered question.
    async fn consult_bridge_lane(
        &self,
        input: &RetrievalFeedbackInput,
        specs: &[FeedbackEdgeSpec],
    ) -> BridgeAction {
        use ansible_mesh_core::autonomy::LANE_GRAPH_BRIDGE_EDGES;

        let anchor = specs
            .first()
            .map(|s| s.from_id.as_str())
            .unwrap_or_default();
        let action_summary = format!(
            "bridge {} RELATES_TO edge(s) from '{}' for {:?} recall feedback {}",
            specs.len(),
            anchor,
            input.rating,
            input.feedback_id,
        );
        let evidence = format!(
            "feedback_id={} packet_id={} rating={:?} query_summary={:?} \
             connectivity_ratio={:?} anchor={} targets=[{}]",
            input.feedback_id,
            input.packet_id,
            input.rating,
            input.query_summary.as_deref().unwrap_or(""),
            input.connectivity_ratio(),
            anchor,
            specs
                .iter()
                .map(|s| s.to_id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        );
        let reversal_hint = format!(
            "MATCH ()-[r:RELATES_TO {{feedback_signal_id: '{}'}}]-() DELETE r",
            input.feedback_id
        );

        match self
            .autonomy
            .consume_action(
                LANE_GRAPH_BRIDGE_EDGES,
                &action_summary,
                &evidence,
                &reversal_hint,
            )
            .await
        {
            Ok(decision) => plan_bridge_action(&decision),
            Err(e) => {
                warn!(
                    feedback_id = %input.feedback_id,
                    "feedback-to-action: autonomy consult failed, staying prose-only: {e:#}"
                );
                BridgeAction::ProseOnly {
                    reason: "autonomy_unavailable".to_string(),
                }
            }
        }
    }

    /// Execute the compiled bridge-edge MERGEs. Missing endpoints write
    /// nothing and are reported rather than failing (mirrors the
    /// `life.observe` edge path). Returns `(edges_written, missing_targets)`.
    async fn execute_bridge_edges(
        graph: &Graph,
        specs: &[FeedbackEdgeSpec],
    ) -> Result<(usize, Vec<String>)> {
        let mut written = 0usize;
        let mut missing = Vec::new();
        for spec in specs {
            let compiled = cypher::compile_feedback_bridge_edge(spec)
                .map_err(|e| anyhow::anyhow!("bridge edge Cypher compilation failed: {e}"))?;
            let mut rows = graph
                .execute(
                    query(&compiled.query)
                        .param("from_id", compiled.from_id.as_str())
                        .param("to_id", compiled.to_id.as_str())
                        .param("created_at", compiled.created_at.as_str())
                        .param("created_by", compiled.created_by.as_str())
                        .param("feedback_signal_id", compiled.feedback_signal_id.as_str()),
                )
                .await?;
            match rows.next().await {
                Ok(Some(_)) => {
                    written += 1;
                    info!(
                        from_id = %spec.from_id,
                        to_id = %spec.to_id,
                        feedback_signal_id = %spec.feedback_signal_id,
                        "feedback-to-action: bridge edge merged"
                    );
                }
                Ok(None) => {
                    warn!(
                        from_id = %spec.from_id,
                        to_id = %spec.to_id,
                        "feedback-to-action: bridge edge endpoint not found; edge skipped"
                    );
                    missing.push(spec.to_id.clone());
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("bridge edge MERGE failed: {e}"));
                }
            }
        }
        Ok((written, missing))
    }

    /// Handle `life.patch.apply` — the confirmation actuator for
    /// `awaiting_confirmation` patches (Autopoiesis Slice A2).
    ///
    /// Confirm: applies the embedded edge specs, marks the patch `applied`,
    /// and reports `confirmed_good` against the patch's hotel audit id so
    /// the `graph.bridge_edges` lane earns toward promotion. Reject: marks
    /// the patch `rejected` and reports `reversed` (demotes the lane).
    async fn handle_patch_apply(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        let input: LifePatchApplyInput = serde_json::from_value(task.parameters.clone())
            .context("failed to parse life.patch.apply parameters as LifePatchApplyInput")?;
        if let Err(e) = input.validate() {
            return Ok(ProviderOutput::ResultSet(json!({
                "status": "blocked",
                "reasons": e.violations,
            })));
        }

        let graph = self.connect().await?;
        let mut rows = graph
            .execute(query(cypher::patch_lookup_query()).param("patch_id", input.patch_id.as_str()))
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(ProviderOutput::ResultSet(json!({
                "status": "not_found",
                "patch_id": input.patch_id,
            })));
        };
        let current_status: String = row.get("status").unwrap_or_default();
        let patch_json: String = row.get("patch_json").unwrap_or_default();
        if current_status != cypher::PATCH_STATUS_AWAITING_CONFIRMATION {
            return Ok(ProviderOutput::ResultSet(json!({
                "status": "not_applicable",
                "patch_id": input.patch_id,
                "current_status": current_status,
            })));
        }
        let patch: LifePatchProposalInput = serde_json::from_str(&patch_json)
            .context("life.patch.apply: stored patch_json failed to parse")?;

        let now = chrono::Utc::now().to_rfc3339();
        let (new_status, edges_written, missing_targets, outcome) = match input.decision {
            PatchApplyDecision::Confirm => {
                let (written, missing) =
                    Self::execute_bridge_edges(&graph, &patch.edge_specs).await?;
                (
                    cypher::PATCH_STATUS_APPLIED,
                    written,
                    missing,
                    "confirmed_good",
                )
            }
            PatchApplyDecision::Reject => {
                (cypher::PATCH_STATUS_REJECTED, 0, Vec::new(), "reversed")
            }
        };

        let mut rows = graph
            .execute(
                query(cypher::patch_status_update_query())
                    .param("patch_id", input.patch_id.as_str())
                    .param("status", new_status)
                    .param("updated_at", now.as_str()),
            )
            .await?;
        let _ = rows.next().await?;

        // Report the outcome to the hotel so the lane earns (or demotes).
        // Best-effort: the graph mutation above is the operator-visible
        // truth; a failed report is surfaced in the response for retry.
        let mut outcome_reported = false;
        if let Some(audit_id) = patch.autonomy_audit_id.as_deref() {
            match self.autonomy.record_outcome(audit_id, outcome).await {
                Ok(data) => {
                    outcome_reported = data
                        .get("recorded")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let transition = data
                        .get("transition")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    info!(
                        patch_id = %input.patch_id,
                        audit_id,
                        outcome,
                        transition = %transition,
                        "life.patch.apply: autonomy outcome recorded"
                    );
                }
                Err(e) => {
                    warn!(
                        patch_id = %input.patch_id,
                        audit_id,
                        "life.patch.apply: autonomy outcome report failed: {e:#}"
                    );
                }
            }
        }

        Ok(ProviderOutput::ResultSet(json!({
            "status": new_status,
            "patch_id": input.patch_id,
            "edges_written": edges_written,
            "missing_targets": missing_targets,
            "audit_id": patch.autonomy_audit_id,
            "outcome": outcome,
            "outcome_reported": outcome_reported,
        })))
    }

    /// Handle `life.patch.list` — the READ-ONLY patch review surface.
    ///
    /// Lists governed patch proposals (default: the pending set) with their
    /// risk tier, gate, lifecycle status, audit anchor, and a compact
    /// provenance summary drawn from the stored `patch_json`. This handler
    /// issues only a single read query and never mutates the graph.
    async fn handle_patch_list(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        let input: LifePatchListInput =
            serde_json::from_value(task.parameters.clone()).unwrap_or_default();
        let statuses = input.effective_statuses();
        let limit = input.effective_limit();
        let cypher = cypher::patch_list_query(&statuses, limit);

        let graph = self.connect().await?;
        let mut rows = graph.execute(query(&cypher)).await?;
        let mut patches = Vec::new();
        while let Some(row) = rows.next().await? {
            let risk: String = row.get("risk").unwrap_or_default();
            let gate = serde_json::from_value::<PatchRisk>(json!(risk))
                .ok()
                .map(|r| r.gate());
            let patch_json: String = row.get("patch_json").unwrap_or_default();
            let provenance = patch_provenance_summary(&patch_json);
            patches.push(json!({
                "patch_id": opt_str(row.get("patch_id").unwrap_or_default()),
                "patch_kind": opt_str(row.get("patch_kind").unwrap_or_default()),
                "risk": opt_str(risk),
                "gate": gate,
                "status": opt_str(row.get("status").unwrap_or_default()),
                "summary": opt_str(row.get("summary").unwrap_or_default()),
                "rationale": opt_str(row.get("rationale").unwrap_or_default()),
                "proposed_at": opt_str(row.get("proposed_at").unwrap_or_default()),
                "status_updated_at": opt_str(row.get("status_updated_at").unwrap_or_default()),
                "autonomy_audit_id": opt_str(row.get("autonomy_audit_id").unwrap_or_default()),
                "provenance": provenance,
            }));
        }

        Ok(ProviderOutput::ResultSet(json!({
            "status": "ok",
            "read_only": true,
            "statuses": statuses,
            "limit": limit,
            "count": patches.len(),
            "patches": patches,
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

    /// Dedicated pipeline for `cross_domain_entanglement`: score candidates
    /// against BOTH domain embeddings, keep the intersection above threshold
    /// (ranked by `min(score_a, score_b)`), then discover living-cycle bridge
    /// nodes reachable from a strong hit on each side. Every hit in the
    /// packet is labeled with `entanglement_kind` and a human-readable
    /// `entanglement_reason` saying WHY it is entangled.
    async fn handle_cross_domain_recall(
        &self,
        task: &DatasourceTask,
        query_val: &RetrievalQuery,
        fallback_embedding: &[f32],
        now_iso: &str,
    ) -> Result<ProviderOutput> {
        let domain_a_embedding = embedding_from_key(&task.parameters, "domain_a_embedding")
            .unwrap_or_else(|| fallback_embedding.to_vec());
        let domain_b_embedding = embedding_from_key(&task.parameters, "domain_b_embedding")
            .unwrap_or_else(|| fallback_embedding.to_vec());

        // Dual sweep: the SAME candidate labels are scored against each
        // domain embedding so the intersection is well-defined.
        let per_label_top_k = (query_val.max_context_packets * 2).max(8);
        let mut hits_a = Vec::new();
        let mut hits_b = Vec::new();
        for (space, label) in entanglement::candidate_spaces() {
            self.extend_vector_hits(
                &mut hits_a,
                space.clone(),
                &[label],
                per_label_top_k,
                entanglement::CROSS_DOMAIN_SEARCH_FLOOR,
                &domain_a_embedding,
            )
            .await?;
            self.extend_vector_hits(
                &mut hits_b,
                space,
                &[label],
                per_label_top_k,
                entanglement::CROSS_DOMAIN_SEARCH_FLOOR,
                &domain_b_embedding,
            )
            .await?;
        }
        let domain_a_sweep = hits_a.len();
        let domain_b_sweep = hits_b.len();

        let intersection = entanglement::intersect_domain_hits(
            hits_a,
            hits_b,
            entanglement::CROSS_DOMAIN_MIN_SIMILARITY,
        );

        // Bridge discovery: one living-cycle hop from a strong domain-A hit
        // AND a strong domain-B hit. Failures degrade to vector-only
        // entanglement — never fail the recall.
        let anchors_a = intersection.domain_a_anchors();
        let anchors_b = intersection.domain_b_anchors();
        let mut bridges = Vec::new();
        if !anchors_a.is_empty() && !anchors_b.is_empty() {
            let a_ids: Vec<&str> = anchors_a.keys().map(String::as_str).collect();
            let b_ids: Vec<&str> = anchors_b.keys().map(String::as_str).collect();
            let cypher = entanglement::bridge_discovery_cypher(&a_ids, &b_ids, 16);
            match self.execute_cypher(&cypher).await {
                Ok(result) => {
                    let rows = entanglement::parse_bridge_rows(&result);
                    bridges = entanglement::fold_bridge_hits(
                        rows,
                        &anchors_a,
                        &anchors_b,
                        entanglement::BRIDGE_SCORE_DECAY,
                    );
                }
                Err(e) => {
                    warn!(
                        query_id = %query_val.query_id,
                        "cross_domain_entanglement: bridge discovery failed; \
                         returning vector-only entanglement: {e}"
                    );
                }
            }
        }

        let candidates = entanglement::assemble_entangled_candidates(
            intersection,
            bridges,
            entanglement::MAX_SINGLE_DOMAIN_CONTEXT_HITS,
        );

        // Policy filters apply per candidate exactly as elsewhere; the
        // entanglement score is authoritative (no re-ranking by weights).
        let filters = &query_val.policy_filters;
        let mut metadata_by_id: std::collections::HashMap<String, Value> =
            std::collections::HashMap::new();
        let mut explanations = Vec::new();
        let mut kind_counts: std::collections::HashMap<&'static str, usize> =
            std::collections::HashMap::new();
        let mut scored: Vec<projection::ScoredHit> = Vec::new();
        for candidate in candidates {
            let (surviving, _drop_log) =
                projection::apply_policy_filters(vec![candidate.hit.clone()], filters);
            if surviving.is_empty() {
                continue;
            }
            *kind_counts.entry(candidate.kind.as_str()).or_insert(0) += 1;
            let node_id = candidate.hit.node_id().to_string();
            metadata_by_id.insert(node_id.clone(), candidate.metadata());
            explanations.push(json!({
                "node_id": node_id,
                "title": candidate.hit.title(),
                "label": candidate.hit.label,
                "entanglement_kind": candidate.kind.as_str(),
                "reason": candidate.reason(),
            }));
            let expansion_origin = if candidate.kind == entanglement::EntanglementKind::Bridge {
                candidate
                    .domain_a_anchors
                    .first()
                    .map(|anchor| projection::ExpansionOrigin {
                        origin: GraphRecordRef {
                            id: anchor.id.clone(),
                            label: anchor.label.clone(),
                            datasource: Some("life-graph".into()),
                        },
                        rel_type: candidate
                            .bridge_a_rel_types
                            .first()
                            .cloned()
                            .unwrap_or_else(|| "RELATES_TO".to_string()),
                    })
            } else {
                None
            };
            scored.push(projection::ScoredHit {
                score: candidate.score,
                matched_policy_filters: Vec::new(),
                expansion_origin,
                fallback_origin: false,
                hit: candidate.hit,
            });
        }

        let context_id = format!("ctx:{}", query_val.query_id);
        let token_budget = query_val.max_context_packets * 200;
        let mut packet = projection::project_context_packet(
            &context_id,
            &query_val.query_id,
            query_val.strategy.clone(),
            scored,
            Vec::new(),
            token_budget,
            now_iso,
        );
        entanglement::annotate_packet(&mut packet, &metadata_by_id);

        info!(
            query_id = %query_val.query_id,
            result_count = packet.ranked_packets.len(),
            semantic_both = kind_counts.get("semantic_both").copied().unwrap_or(0),
            bridge = kind_counts.get("bridge").copied().unwrap_or(0),
            domain_a_strong = anchors_a.len(),
            domain_b_strong = anchors_b.len(),
            "cross_domain_entanglement: context packet projected"
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
            "named_strategy": NamedRecallStrategy::CrossDomainEntanglement.as_str(),
            "fallback_used": FallbackUsage::None.as_str(),
            "entanglement": {
                "threshold": entanglement::CROSS_DOMAIN_MIN_SIMILARITY,
                "domain_a_sweep_hits": domain_a_sweep,
                "domain_b_sweep_hits": domain_b_sweep,
                "domain_a_strong_hits": anchors_a.len(),
                "domain_b_strong_hits": anchors_b.len(),
                "counts": {
                    "semantic_both": kind_counts.get("semantic_both").copied().unwrap_or(0),
                    "bridge": kind_counts.get("bridge").copied().unwrap_or(0),
                    "domain_a_only": kind_counts.get("domain_a_only").copied().unwrap_or(0),
                    "domain_b_only": kind_counts.get("domain_b_only").copied().unwrap_or(0),
                },
                "explanations": explanations,
            },
            "context_packet": packet_json,
            "cross_agent_context_packet": cross_agent_packet_json,
        })))
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

/// Map an empty string (a missing Memgraph property read as `""`) to JSON
/// null so the read-only patch listing does not fabricate empty values.
fn opt_str(s: String) -> Value {
    if s.is_empty() {
        Value::Null
    } else {
        Value::String(s)
    }
}

/// Compact, bounded provenance summary derived from a patch node's stored
/// `patch_json`. Surfaces evidence count, contributing sources, operator
/// approval, and embedded edge-spec count for the review surface without
/// dumping the full patch payload.
fn patch_provenance_summary(patch_json: &str) -> Value {
    let Ok(value) = serde_json::from_str::<Value>(patch_json) else {
        return json!({ "parse_error": true });
    };
    let packets = value.get("evidence_packets").and_then(Value::as_array);
    let evidence_count = packets.map(|p| p.len()).unwrap_or(0);
    let mut sources = Vec::new();
    if let Some(packets) = packets {
        for packet in packets {
            if let Some(refs) = packet.get("source_refs").and_then(Value::as_array) {
                for source in refs {
                    let source_id = source
                        .get("source_id")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if source_id.is_empty() {
                        continue;
                    }
                    sources.push(json!({
                        "source_id": source_id,
                        "source_kind": source.get("source_kind").and_then(Value::as_str),
                    }));
                    if sources.len() >= 8 {
                        break;
                    }
                }
            }
            if sources.len() >= 8 {
                break;
            }
        }
    }
    json!({
        "evidence_count": evidence_count,
        "sources": sources,
        "operator_approved": value
            .get("operator_approved")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "edge_spec_count": value
            .get("edge_specs")
            .and_then(Value::as_array)
            .map(|specs| specs.len())
            .unwrap_or(0),
    })
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
        edge_specs: Vec::new(),
        autonomy_audit_id: None,
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

    // ── Recall similarity gate (const + env override) ─────────────────────

    #[test]
    fn recall_min_similarity_defaults_when_unset_or_blank() {
        assert!(
            (parse_recall_min_similarity(None) - DEFAULT_RECALL_MIN_SIMILARITY).abs()
                < f32::EPSILON
        );
        assert!(
            (parse_recall_min_similarity(Some("")) - DEFAULT_RECALL_MIN_SIMILARITY).abs()
                < f32::EPSILON
        );
        assert!(
            (parse_recall_min_similarity(Some("   ")) - DEFAULT_RECALL_MIN_SIMILARITY).abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn recall_min_similarity_parses_override() {
        assert!((parse_recall_min_similarity(Some("0.5")) - 0.5).abs() < f32::EPSILON);
        assert!((parse_recall_min_similarity(Some(" 0.25 ")) - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn recall_min_similarity_clamps_to_valid_band() {
        assert!((parse_recall_min_similarity(Some("2.0")) - 0.9).abs() < f32::EPSILON);
        assert!(parse_recall_min_similarity(Some("-1")).abs() < f32::EPSILON);
    }

    #[test]
    fn recall_min_similarity_invalid_falls_back_to_default() {
        for invalid in ["high", "0.4.2", "NaN", "inf"] {
            assert!(
                (parse_recall_min_similarity(Some(invalid)) - DEFAULT_RECALL_MIN_SIMILARITY).abs()
                    < f32::EPSILON,
                "{invalid:?} should fall back to the default gate"
            );
        }
    }

    #[test]
    fn recall_min_similarity_reads_env_override() {
        // Only this test touches RECALL_MIN_SIMILARITY_ENV; nothing else in
        // this crate's tests reads the environment concurrently.
        unsafe { std::env::set_var(RECALL_MIN_SIMILARITY_ENV, "0.25") };
        assert!((recall_min_similarity_from_env() - 0.25).abs() < f32::EPSILON);
        unsafe { std::env::remove_var(RECALL_MIN_SIMILARITY_ENV) };
        assert!(
            (recall_min_similarity_from_env() - DEFAULT_RECALL_MIN_SIMILARITY).abs() < f32::EPSILON
        );
    }

    // ── Fallback blend ─────────────────────────────────────────────────────

    fn scored_tuple(id: &str, score: f32) -> ScoredTuple {
        (
            projection::VectorHit {
                bolt_id: 1,
                label: "OpenLoop".to_string(),
                properties: json!({
                    "id": id,
                    "title": "loop",
                    "confidence": 0.7,
                    "validation_state": "proposed",
                    "status": "open"
                }),
                similarity: 0.5,
            },
            score,
            Vec::new(),
        )
    }

    #[test]
    fn fallback_usage_serializes_tri_state() {
        assert_eq!(FallbackUsage::None.as_str(), "false");
        assert_eq!(FallbackUsage::ToppedUp.as_str(), "topped_up");
        assert_eq!(FallbackUsage::Full.as_str(), "full_fallback");
    }

    #[test]
    fn blend_uses_vector_hits_only_when_enough() {
        let vector = vec![
            scored_tuple("v1", 0.8),
            scored_tuple("v2", 0.6),
            scored_tuple("v3", 0.5),
        ];
        let (out, usage) = blend_with_fallback(vector, vec![scored_tuple("f1", 0.9)], 3);
        assert_eq!(usage, FallbackUsage::None);
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|s| !s.fallback_origin));
    }

    #[test]
    fn blend_tops_up_below_weakest_vector_hit() {
        let vector = vec![scored_tuple("v1", 0.8), scored_tuple("v2", 0.4)];
        let fallback = vec![
            scored_tuple("v2", 0.9), // duplicate node id: deduped
            scored_tuple("f1", 0.7),
            scored_tuple("f2", 0.3),
            scored_tuple("f3", 0.2),
        ];
        let (out, usage) = blend_with_fallback(vector, fallback, 4);

        assert_eq!(usage, FallbackUsage::ToppedUp);
        // 2 vector hits + top-up limited to (max_context_packets - 2).
        assert_eq!(out.len(), 4);
        assert!(!out[0].fallback_origin && !out[1].fallback_origin);
        assert!(out[2].fallback_origin && out[3].fallback_origin);
        // The duplicate never appears twice.
        assert_eq!(out.iter().filter(|s| s.hit.node_id() == "v2").count(), 1);
        // Top-up rows rank strictly below the weakest vector hit, with
        // their relative order preserved (f1 above f2).
        assert!(out[2].score < out[1].score);
        assert_eq!(out[2].hit.node_id(), "f1");
        assert!(out[2].score > out[3].score);
        assert_eq!(out[3].hit.node_id(), "f2");
    }

    #[test]
    fn blend_all_duplicate_fallback_is_not_counted_as_topped_up() {
        let vector = vec![scored_tuple("v1", 0.8)];
        let fallback = vec![scored_tuple("v1", 0.9)];
        let (out, usage) = blend_with_fallback(vector, fallback, 3);
        assert_eq!(usage, FallbackUsage::None);
        assert_eq!(out.len(), 1);
        assert!(!out[0].fallback_origin);
    }

    #[test]
    fn blend_full_fallback_when_zero_vector_hits() {
        let fallback = vec![scored_tuple("f1", 0.3), scored_tuple("f2", 0.5)];
        let (out, usage) = blend_with_fallback(Vec::new(), fallback, 3);
        assert_eq!(usage, FallbackUsage::Full);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|s| s.fallback_origin));
        // Sorted descending by score.
        assert_eq!(out[0].hit.node_id(), "f2");
    }

    #[test]
    fn blend_empty_everything_reports_no_fallback() {
        let (out, usage) = blend_with_fallback(Vec::new(), Vec::new(), 3);
        assert!(out.is_empty());
        assert_eq!(usage, FallbackUsage::None);
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
            query_context_ref: None,
            connected_candidate_refs: vec![],
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
    fn patch_provenance_summary_extracts_sources_and_counts() {
        let patch_json = json!({
            "patch_id": "patch:recall-feedback:f1",
            "operator_approved": false,
            "edge_specs": [],
            "evidence_packets": [{
                "source_refs": [
                    {"source_id": "agent:memorygraphrag", "source_kind": "runtime_observation"},
                    {"source_id": "", "source_kind": "ignored_empty"}
                ]
            }]
        })
        .to_string();
        let summary = patch_provenance_summary(&patch_json);
        assert_eq!(summary["evidence_count"], 1);
        assert_eq!(summary["operator_approved"], false);
        assert_eq!(summary["edge_spec_count"], 0);
        let sources = summary["sources"].as_array().expect("sources array");
        assert_eq!(sources.len(), 1, "empty source_id must be dropped");
        assert_eq!(sources[0]["source_id"], "agent:memorygraphrag");
    }

    #[test]
    fn patch_provenance_summary_flags_unparseable_json() {
        let summary = patch_provenance_summary("not json");
        assert_eq!(summary["parse_error"], true);
    }

    #[test]
    fn opt_str_maps_empty_to_null() {
        assert_eq!(opt_str(String::new()), Value::Null);
        assert_eq!(opt_str("x".into()), Value::String("x".into()));
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

    // ── Feedback-to-action gating (Autopoiesis Slice A2) ─────────────────────

    fn decision(allowed: bool, posture: Option<&str>, audit_id: Option<&str>) -> AutonomyDecision {
        AutonomyDecision {
            allowed,
            posture: posture.map(str::to_string),
            audit_id: audit_id.map(str::to_string),
            reason: None,
        }
    }

    #[test]
    fn plan_bridge_action_gates_by_posture() {
        // AutoWithAudit: write the edge now.
        assert_eq!(
            plan_bridge_action(&decision(true, Some("auto_with_audit"), Some("a1"))),
            BridgeAction::ApplyNow {
                audit_id: "a1".into()
            }
        );

        // ConfirmFirst: do NOT write — file the ready-to-apply spec.
        assert_eq!(
            plan_bridge_action(&decision(false, Some("confirm_first"), Some("a2"))),
            BridgeAction::AwaitConfirmation {
                audit_id: "a2".into()
            }
        );

        // ProposalOnly (day one for every fresh grant): prose-only, as today.
        assert_eq!(
            plan_bridge_action(&AutonomyDecision {
                allowed: false,
                posture: Some("proposal_only".into()),
                audit_id: None,
                reason: Some("posture_proposal_only".into()),
            }),
            BridgeAction::ProseOnly {
                reason: "posture_proposal_only".into()
            }
        );
    }

    #[test]
    fn plan_bridge_action_fails_safe_on_malformed_decisions() {
        // allowed=true without an audit anchor must NOT write.
        assert!(matches!(
            plan_bridge_action(&decision(true, Some("auto_with_audit"), None)),
            BridgeAction::ProseOnly { .. }
        ));
        // confirm_first without an audit id cannot await a confirmation.
        assert!(matches!(
            plan_bridge_action(&decision(false, Some("confirm_first"), None)),
            BridgeAction::ProseOnly { .. }
        ));
        // allowed=true with a non-auto posture is inconsistent — refuse.
        assert!(matches!(
            plan_bridge_action(&decision(true, Some("confirm_first"), Some("a1"))),
            BridgeAction::ProseOnly { .. }
        ));
        // Refusal reasons surface for the response envelope.
        assert_eq!(
            plan_bridge_action(&AutonomyDecision {
                allowed: false,
                posture: None,
                audit_id: None,
                reason: Some("lane_disabled".into()),
            }),
            BridgeAction::ProseOnly {
                reason: "lane_disabled".into()
            }
        );
        assert_eq!(
            plan_bridge_action(&AutonomyDecision {
                allowed: false,
                posture: None,
                audit_id: None,
                reason: None,
            }),
            BridgeAction::ProseOnly {
                reason: "no_grant".into()
            }
        );
    }

    #[test]
    fn autonomy_decision_parses_hotel_response_data() {
        let parsed = AutonomyDecision::from_response_data(&json!({
            "allowed": false,
            "posture": "confirm_first",
            "audit_id": "autonomy:graph.bridge_edges:abc",
        }));
        assert_eq!(
            parsed,
            decision(
                false,
                Some("confirm_first"),
                Some("autonomy:graph.bridge_edges:abc")
            )
        );

        // Missing / empty fields degrade safely.
        let parsed = AutonomyDecision::from_response_data(&json!({}));
        assert!(!parsed.allowed);
        assert_eq!(parsed.posture, None);
        assert_eq!(parsed.audit_id, None);
        assert!(matches!(
            plan_bridge_action(&parsed),
            BridgeAction::ProseOnly { .. }
        ));
    }

    #[test]
    fn disconnected_feedback_with_anchor_derives_specs_but_missing_variant_respects_refs() {
        // The provider derives edge specs only for SafeAutoUpdate ratings
        // that carry an anchor + candidates; the fixture has an anchor but
        // no connected_candidate_refs, so disconnected derives nothing.
        let mut feedback = feedback_input(RetrievalFeedbackRating::Disconnected);
        feedback.query_context_ref = Some("life:open_loop:anchor".into());
        assert!(feedback_edge_specs(&feedback, "2026-07-07T00:00:00Z").is_empty());

        feedback.connected_candidate_refs = vec!["life:project:phi".into()];
        let specs = feedback_edge_specs(&feedback, "2026-07-07T00:00:00Z");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].to_id, "life:project:phi");

        // Missing feedback bridges to its missing_context_refs.
        let mut feedback = feedback_input(RetrievalFeedbackRating::Missing);
        feedback.query_context_ref = Some("life:open_loop:anchor".into());
        let specs = feedback_edge_specs(&feedback, "2026-07-07T00:00:00Z");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].to_id, "life:goal:graph");
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
