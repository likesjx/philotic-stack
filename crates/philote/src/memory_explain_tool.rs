//! Memory Transparency Slice M2 (`docs/architecture/MEMORY_TRANSPARENCY_PROPOSAL.md`):
//! the `memory.explain` tool — "why do you believe X?" fanned out across the
//! three memory planes and merged into one provenance-aware report.
//!
//! Transport per plane (each is a deliberate, named choice — see the M2
//! disposition in the proposal doc for the reality-gap writeup):
//!
//! - **Muninn**: direct REST via the existing `MuninnRestEngine::activate`
//!   (the same call `memory.recall` already makes), bounded by a local
//!   timeout so a slow/unreachable MuninnDB degrades this plane instead of
//!   hanging the whole tool call.
//! - **Intel graph**: direct REST `GET /api/mutations` against the
//!   graph-intelligence server (`PHILOTIC_INTEL_GRAPH_URL`, default
//!   `http://127.0.0.1:8900`), filtered client-side by claim substring — the
//!   server has no full-text search over decision `reason` text today.
//! - **LifeGraph**: `life.recall` is mesh-routed and asynchronous (see
//!   `dispatch_life_recall_prefetch` in `memory_integration.rs` — the runner
//!   may live on a different hotel entirely). There is no synchronous
//!   request/response path a tool call can block on in this slice, so this
//!   plane reads the session's existing prefetch cache
//!   (`SessionState::life_recall_cache`) instead of issuing a fresh query.
//!   Evidence is therefore whatever the last prefetch returned, not a live
//!   per-claim recall — a real, named limitation, not a silent one.

use super::*;
use ansible_mesh_core::memory_explain::{
    ExplainEvidenceItem, ExplainPlane, ExplainPlaneOutcome, ExplainReport, band_report,
    envelope_from_decision_details, envelope_from_engram_metadata,
    muninn_activate_timestamp_to_unix_seconds, render_text,
};
use ansible_mesh_core::provenance::{ProvenanceEnvelope, TrustTier};

const INTEL_GRAPH_DEFAULT_URL: &str = "http://127.0.0.1:8900";
const MUNINN_PLANE_TIMEOUT: Duration = Duration::from_secs(8);
const INTEL_GRAPH_PLANE_TIMEOUT: Duration = Duration::from_secs(5);
const INTEL_GRAPH_MAX_MATCHES: usize = 10;

impl AgentRuntime {
    pub(super) async fn execute_memory_explain_tool(
        &mut self,
        payload: ToolExecutionPayload,
    ) -> Result<()> {
        let claim = payload
            .arguments
            .get("claim")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        if claim.is_empty() {
            return self
                .fail_active_turn(
                    payload.session_id,
                    payload.turn_id,
                    "memory.explain: 'claim' is required.".into(),
                )
                .await;
        }

        let limit = payload
            .arguments
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(8)
            .clamp(1, 20);
        let entity = payload
            .arguments
            .get("entity")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let memory_user_id = self
            .sessions
            .get(&payload.session_id)
            .map(turn_memory_user_id)
            .unwrap_or_else(|| self.agent_id.clone());

        let muninn_outcome = self
            .fetch_muninn_explain_plane(&claim, &memory_user_id, limit)
            .await;
        let intel_graph_outcome = fetch_intel_graph_explain_plane(&claim, entity.as_deref()).await;
        let lifegraph_outcome = self.fetch_lifegraph_explain_plane(&payload.session_id, &claim);

        let report = ExplainReport {
            claim: claim.clone(),
            planes: vec![muninn_outcome, intel_graph_outcome, lifegraph_outcome],
        };
        let content = render_text(&band_report(&report));

        self.handle_tool_result(InboundTaskPayload {
            action: Some("tool_result".into()),
            agent_action: None,
            handoff_bundle: None,
            source: Some("agent".into()),
            session_id: Some(payload.session_id),
            turn_id: Some(payload.turn_id),
            transport: None,
            chat_id: Some(payload.chat_id),
            thread_id: None,
            sender_id: None,
            sender_username: None,
            message_kind: None,
            content: Some(content),
            attachments: Vec::new(),
            command: None,
            callback_data: None,
            raw_transport_event: None,
            error: None,
            tool_name: Some(payload.tool_name),
            arguments: None,
            final_reply_to: Some(payload.final_reply_to),
            final_reply_role: Some(payload.final_reply_role),
            final_reply_guest_id: payload.final_reply_guest_id,
            ..Default::default()
        })
        .await
    }

    async fn fetch_muninn_explain_plane(
        &self,
        claim: &str,
        memory_user_id: &str,
        limit: usize,
    ) -> ExplainPlaneOutcome {
        use memory_core::MemoryEngine as _;

        let Some(engine) = self.memory_engine_for(&self.agent_id, memory_user_id) else {
            return ExplainPlaneOutcome::unavailable(
                ExplainPlane::Muninn,
                "MuninnDB not configured or the hotel has reported the endpoint down",
            );
        };
        // Cross-scope: an explain query does not know a priori whether the
        // belief lives in the agent's own vault or the shared-user vault.
        // Vaults without a token are silently skipped by `activate` (not an
        // error) — see `MuninnRestEngine::activate`'s cross-scope handling.
        let scope = MemoryScope::CrossScope(vec![MemoryScope::SelfOnly, MemoryScope::SharedUser]);

        match tokio::time::timeout(
            MUNINN_PLANE_TIMEOUT,
            engine.activate(claim, scope, Some(limit)),
        )
        .await
        {
            Err(_) => ExplainPlaneOutcome::unavailable(
                ExplainPlane::Muninn,
                format!("activate() did not respond within {MUNINN_PLANE_TIMEOUT:?}"),
            ),
            Ok(Err(e)) => ExplainPlaneOutcome::unavailable(
                ExplainPlane::Muninn,
                format!("activate() failed: {e}"),
            ),
            Ok(Ok(result)) => {
                let items = result
                    .engrams
                    .into_iter()
                    .map(|eng| ExplainEvidenceItem {
                        plane: ExplainPlane::Muninn,
                        label: eng.concept.clone(),
                        detail: eng.content.clone(),
                        source_ref: Some(eng.id.clone()),
                        // `/api/activate` returns nanosecond-epoch timestamps
                        // (unlike `/api/engrams`'s second-epoch) — normalize
                        // so cross-plane recency sort compares like units.
                        recorded_at: Some(muninn_activate_timestamp_to_unix_seconds(
                            eng.updated_at as i64,
                        )),
                        envelope: envelope_from_engram_metadata(&eng.metadata),
                    })
                    .collect();
                ExplainPlaneOutcome::ok(ExplainPlane::Muninn, items)
            }
        }
    }

    /// LifeGraph plane: no synchronous transport exists (see module doc) —
    /// read the session's existing `life.recall` prefetch cache instead of
    /// issuing a fresh query. Distinguishes three honest states: no route
    /// bound (LifeGraph not enabled for this profile), route bound but the
    /// cache has never been populated (runner never responded), and route
    /// bound with cached records (filtered by claim substring, possibly to
    /// zero matches — a real "the plane answered, found nothing" outcome).
    fn fetch_lifegraph_explain_plane(&self, session_id: &str, claim: &str) -> ExplainPlaneOutcome {
        let Some(state) = self.sessions.get(session_id) else {
            return ExplainPlaneOutcome::unavailable(
                ExplainPlane::LifeGraph,
                "no session state found",
            );
        };
        if state.resolve_tool_route("life.recall").is_none() {
            return ExplainPlaneOutcome::unavailable(
                ExplainPlane::LifeGraph,
                "no life.recall route bound for this profile",
            );
        }
        if state.life_recall_cache.is_empty() {
            return ExplainPlaneOutcome::unavailable(
                ExplainPlane::LifeGraph,
                "life.recall prefetch cache is empty for this session — the LifeGraph runner has \
                 not returned a result yet (this plane reads the async prefetch cache, not a live \
                 per-claim query; see module doc)",
            );
        }

        let needle = claim.to_lowercase();
        let now = unix_ts_now();
        let mut items = Vec::new();
        for entry in &state.life_recall_cache {
            for record in &entry.records {
                let haystack = format!("{} {}", record.concept, record.content).to_lowercase();
                if !haystack.contains(&needle) {
                    continue;
                }
                items.push(ExplainEvidenceItem {
                    plane: ExplainPlane::LifeGraph,
                    label: format!(
                        "{} (cached via {} strategy, {}s ago)",
                        record.concept,
                        entry.strategy,
                        now.saturating_sub(entry.fetched_at)
                    ),
                    detail: record.content.clone(),
                    source_ref: record.id.clone(),
                    recorded_at: record.updated_at.or(record.created_at).map(|t| t as i64),
                    envelope: life_recall_record_envelope(record),
                });
            }
        }
        ExplainPlaneOutcome::ok(ExplainPlane::LifeGraph, items)
    }
}

/// Best-effort `ProvenanceEnvelope` from a `RecalledMemoryRecord`'s `trust` /
/// `source` fields (populated by M1's LifeGraph adoption — see
/// `attention_observer` → `n.provenance_envelope` → recall projection).
/// `None` when neither field is present — an honest "pre-provenance record",
/// not a fabricated trust tier.
fn life_recall_record_envelope(record: &RecalledMemoryRecord) -> Option<ProvenanceEnvelope> {
    if record.trust.is_none() && record.source.is_none() {
        return None;
    }
    let trust = record
        .trust
        .as_deref()
        .and_then(|s| match s.to_ascii_lowercase().as_str() {
            "observed" => Some(TrustTier::Observed),
            "inferred" => Some(TrustTier::Inferred),
            "told" => Some(TrustTier::Told),
            _ => None,
        })
        .unwrap_or_default();
    Some(ProvenanceEnvelope {
        source: String::new(),
        author: record.source.clone().unwrap_or_default(),
        trust,
        evidence: Vec::new(),
        reversal: None,
    })
}

/// Intel-graph plane. Two modes:
/// - **Targeted** (`entity` hint present): `GET /api/mutations?target=<entity>`
///   — the server-side exact-match `target_node` filter (see
///   `GraphEngine::get_mutations`), so results are not bounded by the
///   untargeted scan's recency window. This is the fix for a real gap
///   observed live during this slice: an untargeted substring scan over the
///   most-recent 200 mutations missed an older (July 5) decision on
///   `seam:role-handoff-seam` that a targeted query finds immediately.
/// - **Untargeted** (no hint): `GET /api/mutations?limit=200` then a
///   client-side substring filter against `reason`/`target_node`/`action`
///   — the server has no full-text search over decision reason text (only
///   `/api/search` over code/doc nodes). Bounded by the 200-most-recent
///   window; older decisions can age out — a named scope limit, not a bug.
///
/// No ranking sophistication per the M2 slice contract; first
/// `INTEL_GRAPH_MAX_MATCHES` hits, most-recent-first is applied later by the
/// shared `band_report` recency sort.
async fn fetch_intel_graph_explain_plane(claim: &str, entity: Option<&str>) -> ExplainPlaneOutcome {
    let base = std::env::var("PHILOTIC_INTEL_GRAPH_URL")
        .unwrap_or_else(|_| INTEL_GRAPH_DEFAULT_URL.to_string());
    let base = base.trim_end_matches('/').to_string();

    let client = match reqwest::Client::builder()
        .timeout(INTEL_GRAPH_PLANE_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ExplainPlaneOutcome::unavailable(
                ExplainPlane::IntelGraph,
                format!("failed to build HTTP client: {e}"),
            );
        }
    };

    let entity = entity.map(str::trim).filter(|e| !e.is_empty());
    let mut query: Vec<(&str, String)> = Vec::new();
    if let Some(target) = entity {
        query.push(("target", target.to_string()));
        query.push(("limit", "50".to_string()));
    } else {
        query.push(("limit", "200".to_string()));
    }

    let url = format!("{base}/api/mutations");
    let resp = match client.get(&url).query(&query).send().await {
        Ok(r) => r,
        Err(e) => {
            return ExplainPlaneOutcome::unavailable(
                ExplainPlane::IntelGraph,
                format!("request to {url} failed: {e}"),
            );
        }
    };
    if !resp.status().is_success() {
        return ExplainPlaneOutcome::unavailable(
            ExplainPlane::IntelGraph,
            format!("{url} returned HTTP {}", resp.status()),
        );
    }
    let mutations: Vec<serde_json::Value> = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return ExplainPlaneOutcome::unavailable(
                ExplainPlane::IntelGraph,
                format!("failed to parse {url} response: {e}"),
            );
        }
    };

    let needle = claim.to_lowercase();
    let mut items = Vec::new();
    for m in mutations {
        let reason = m.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        let target = m.get("target_node").and_then(|v| v.as_str()).unwrap_or("");
        let action = m.get("action").and_then(|v| v.as_str()).unwrap_or("");
        // A targeted query already narrowed to this exact target_node
        // server-side — trust it. Untargeted scans still need the
        // client-side substring filter over the broad recent window.
        if entity.is_none() {
            let haystack = format!("{reason} {target} {action}").to_lowercase();
            if !haystack.contains(&needle) {
                continue;
            }
        }
        let agent = m
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let details = m.get("details").cloned().unwrap_or(Value::Null);
        let recorded_at = m
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.timestamp());

        items.push(ExplainEvidenceItem {
            plane: ExplainPlane::IntelGraph,
            label: format!("{action} on {target}"),
            detail: reason.to_string(),
            source_ref: m.get("id").and_then(|v| v.as_str()).map(str::to_string),
            recorded_at,
            envelope: envelope_from_decision_details(&details, agent),
        });
        if items.len() >= INTEL_GRAPH_MAX_MATCHES {
            break;
        }
    }
    ExplainPlaneOutcome::ok(ExplainPlane::IntelGraph, items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn life_recall_record_envelope_is_none_without_trust_or_source() {
        let record = RecalledMemoryRecord {
            concept: "x".into(),
            content: "y".into(),
            ..Default::default()
        };
        assert!(life_recall_record_envelope(&record).is_none());
    }

    #[test]
    fn life_recall_record_envelope_maps_trust_string() {
        let record = RecalledMemoryRecord {
            concept: "x".into(),
            content: "y".into(),
            trust: Some("observed".into()),
            source: Some("attention-steward".into()),
            ..Default::default()
        };
        let env = life_recall_record_envelope(&record).expect("envelope");
        assert_eq!(env.trust, TrustTier::Observed);
        assert_eq!(env.author, "attention-steward");
    }

    #[test]
    fn life_recall_record_envelope_defaults_trust_when_unrecognized() {
        let record = RecalledMemoryRecord {
            concept: "x".into(),
            content: "y".into(),
            source: Some("some-writer".into()),
            ..Default::default()
        };
        let env = life_recall_record_envelope(&record).expect("envelope");
        assert_eq!(env.trust, TrustTier::Inferred); // TrustTier::default()
    }
}
