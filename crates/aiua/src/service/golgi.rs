//! Golgi capability-interception pipeline: intercepts inbound agent tasks
//! that match a routing pipeline rule, dispatches them through capability
//! stages (cisternae), and delivers the merged result to the original target.
//! Also hosts the synchronous `CapabilityInvoke` dispatch round-trip.
//!
//! Extracted verbatim from `ipc.rs` — no behavior change.

use super::ipc::{
    CAPABILITY_ROUTER_ROLE, InboxRegistry, IpcServer, PendingCapabilityRegistry,
    agent_graph_db_path, infer_agent_context_for_task, unix_ts,
};
use ansible_mesh_core::agent_graph_storage::SqliteAgentGraphStorage;
use ansible_mesh_core::domain::GraphDomain;
use philotic_client::IpcResponse;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;

/// Special sink role: capabilities reply here when dispatched by the Golgi pipeline.
pub(super) const GOLGI_SINK_ROLE: &str = "hotel:golgi";

/// A task intercepted by the Golgi routing pipeline, awaiting capability-stage completion.
/// Keyed by `"session_id:turn_id"` in [`PendingPipelineRegistry`].
pub(super) struct PendingPipeline {
    pub(super) original_target_role: String,
    pub(super) original_target_guest_id: Option<String>,
    pub(super) original_task_id: Uuid,
    /// Immutable original task JSON — used only for on_failure passthrough.
    pub(super) original_task_json: String,
    /// Evolves as each stage's output is merged in; delivered to the final target.
    pub(super) current_task_json: String,
    pub(super) rule_id: String,
    /// Capability roles yet to execute; front = next stage.
    pub(super) remaining_stages: std::collections::VecDeque<String>,
    /// Enriched provenance records collected so far; written to `golgi_stages`.
    pub(super) completed_stages: Vec<serde_json::Value>,
    /// Zero-based index of the stage currently in flight.
    pub(super) stage_index: usize,
    /// Role of the capability currently in flight (for provenance records).
    pub(super) current_stage_capability: String,
    /// Unix timestamp (seconds) when the current stage was dispatched (for elapsed_ms).
    pub(super) stage_dispatched_at: u64,
    /// Unix timestamp (seconds) when this entry was inserted; used by the TTL watchdog.
    pub(super) created_at: u64,
}

pub(crate) type PendingPipelineRegistry = Arc<Mutex<HashMap<String, PendingPipeline>>>;

impl IpcServer {
    /// Checks whether the inbound task matches any Golgi pipeline rule for the target agent.
    ///
    /// Returns `(capability_role, capability_task_id, capability_task_json)` when intercepted;
    /// stores a [`PendingPipeline`] entry keyed by `"session_id:turn_id"`.
    ///
    /// Only fires on `target_role == "agent"` tasks (via `infer_agent_context_for_task`).
    /// The Park branch is out of scope for Slice 2 — pipeline only applies on Deliver.
    pub(super) async fn try_golgi_intercept(
        graph: &GraphDomain,
        local_node_id: &str,
        target_role: &str,
        target_guest_id: Option<&str>,
        original_task_id: Uuid,
        task_json: &str,
        pending_pipelines: &PendingPipelineRegistry,
    ) -> Option<(String, Uuid, String)> {
        use ansible_mesh_core::agent_graph_storage::AgentGraphStorage as _;

        let ctx = infer_agent_context_for_task(graph, target_role, target_guest_id, task_json)?;

        let path = agent_graph_db_path(&ctx.agent_id);
        if !path.exists() {
            return None;
        }
        let storage = SqliteAgentGraphStorage::open(&ctx.agent_id, &path).ok()?;
        let rules = storage.list_pipeline_rules().ok()?;
        if rules.is_empty() {
            return None;
        }

        let payload: serde_json::Value = serde_json::from_str(task_json).ok()?;
        let task_action = payload
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let session_id = payload
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let turn_id = payload
            .get("turn_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");

        for rule in &rules {
            let rule_json = &rule.rule_json;

            // Match on `rule_json.match.action` — skip rules without explicit action match.
            let match_action = rule_json
                .get("match")
                .and_then(|m| m.get("action"))
                .and_then(serde_json::Value::as_str)?;
            if match_action != task_action {
                continue;
            }

            // Collect all cisternae (pipeline stages) in order.
            let stages = rule_json
                .get("stages")
                .and_then(serde_json::Value::as_array)?;
            if stages.is_empty() {
                continue;
            }
            let first_capability = stages
                .first()?
                .get("capability")
                .and_then(serde_json::Value::as_str)?;
            let remaining: std::collections::VecDeque<String> = stages[1..]
                .iter()
                .filter_map(|s| s.get("capability").and_then(serde_json::Value::as_str))
                .map(str::to_string)
                .collect();

            // Correlation key: session_id:turn_id (unique per in-flight turn).
            let corr_key = format!("{}:{}", session_id, turn_id);

            // Build the capability task: forward the original payload but redirect the
            // reply back to the Golgi sink so the hotel can finalize delivery.
            let mut cap_payload = payload.clone();
            if let Some(obj) = cap_payload.as_object_mut() {
                obj.insert(
                    "reply_role".into(),
                    serde_json::Value::String(GOLGI_SINK_ROLE.into()),
                );
                obj.insert(
                    "reply_to".into(),
                    serde_json::Value::String(local_node_id.into()),
                );
            }
            let cap_json = serde_json::to_string(&cap_payload).ok()?;
            let cap_task_id = Uuid::new_v4();

            {
                let mut guard = pending_pipelines.lock().await;
                // Collision guard: if a pipeline is already in-flight for this turn,
                // refuse the second intercept and fall through to normal delivery.
                if guard.contains_key(&corr_key) {
                    warn!(
                        "Golgi: corr_key '{}' already in-flight — refusing duplicate intercept, \
                         delivering task normally",
                        corr_key
                    );
                    return None;
                }
                let now = unix_ts();
                guard.insert(
                    corr_key,
                    PendingPipeline {
                        original_target_role: target_role.to_string(),
                        original_target_guest_id: target_guest_id.map(str::to_string),
                        original_task_id,
                        original_task_json: task_json.to_string(),
                        current_task_json: task_json.to_string(),
                        rule_id: rule.rule_id.clone(),
                        remaining_stages: remaining,
                        completed_stages: Vec::new(),
                        stage_index: 0,
                        current_stage_capability: first_capability.to_string(),
                        stage_dispatched_at: now,
                        created_at: now,
                    },
                );
            }

            return Some((first_capability.to_string(), cap_task_id, cap_json));
        }

        None
    }

    /// Routes a `CapabilityInvoke` request to the appropriate model-controller guest.
    ///
    /// Finds the best local provider for the task kind, dispatches the request as a
    /// datasource `InboundTask`, awaits the synchronous reply via `pending_capability_calls`,
    /// and returns the result as an `IpcResponse`.
    pub(super) async fn dispatch_capability_invoke(
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        pending_capability_calls: &PendingCapabilityRegistry,
        local_node_id: &str,
        request: ansible_mesh_core::capability::CapabilityRequest,
    ) -> IpcResponse {
        use ansible_mesh_core::capability::CapabilityInput;

        // Find a healthy local provider for this task kind.
        let profiles = match graph.best_model_for(&request.task, local_node_id) {
            Ok(p) => p,
            Err(e) => {
                return IpcResponse::error(
                    "capability_invoke",
                    "INTERNAL_ERROR",
                    &format!("model profile lookup failed: {e}"),
                );
            }
        };

        let profile = match profiles.into_iter().find(|p| p.status == "healthy") {
            Some(p) => p,
            None => {
                return IpcResponse::error(
                    "capability_invoke",
                    "NO_PROVIDER",
                    &format!("no healthy local provider for task '{}'", request.task),
                );
            }
        };

        let target_role = format!("model-controller-{}", profile.model_ref);

        // Build datasource-compatible parameters from CapabilityInputs.
        let mut params = serde_json::Map::new();
        for input in &request.inputs {
            match input {
                CapabilityInput::Image {
                    url,
                    base64,
                    mime_type,
                } => {
                    if let Some(u) = url {
                        params.insert("image_url".into(), serde_json::Value::String(u.clone()));
                    }
                    if let Some(b) = base64 {
                        params.insert("image_base64".into(), serde_json::Value::String(b.clone()));
                    }
                    if let Some(m) = mime_type {
                        params.insert("mime_type".into(), serde_json::Value::String(m.clone()));
                    }
                }
                CapabilityInput::Text { content } => {
                    params.insert("query".into(), serde_json::Value::String(content.clone()));
                }
                CapabilityInput::Audio {
                    url,
                    base64,
                    mime_type,
                } => {
                    if let Some(u) = url {
                        params.insert("audio_url".into(), serde_json::Value::String(u.clone()));
                    }
                    if let Some(b) = base64 {
                        params.insert("audio_base64".into(), serde_json::Value::String(b.clone()));
                    }
                    if let Some(m) = mime_type {
                        params.insert(
                            "audio_mime_type".into(),
                            serde_json::Value::String(m.clone()),
                        );
                    }
                }
                CapabilityInput::Document {
                    url,
                    content,
                    mime_type,
                } => {
                    if let Some(u) = url {
                        params.insert("document_url".into(), serde_json::Value::String(u.clone()));
                    }
                    if let Some(c) = content {
                        params.insert(
                            "document_content".into(),
                            serde_json::Value::String(c.clone()),
                        );
                    }
                    if let Some(m) = mime_type {
                        params.insert(
                            "document_mime_type".into(),
                            serde_json::Value::String(m.clone()),
                        );
                    }
                }
            }
        }

        let task_id = Uuid::new_v4();
        let session_id = request
            .context
            .conversation_id
            .clone()
            .unwrap_or_else(|| task_id.to_string());

        let task_json = serde_json::json!({
            "kind": request.task,
            "parameters": serde_json::Value::Object(params),
            "reply_to": local_node_id,
            "reply_role": CAPABILITY_ROUTER_ROLE,
            "session_id": session_id,
            "turn_id": task_id.to_string(),
            "chat_id": "",
        })
        .to_string();

        // Register oneshot channel before dispatching so there's no race.
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<serde_json::Value, String>>();
        {
            let mut guard = pending_capability_calls.lock().await;
            guard.insert(task_id.to_string(), tx);
        }

        // Deliver InboundTask to the model-controller's inbox subscriber.
        Self::deliver_inbound_task(
            inboxes,
            local_node_id,
            &target_role,
            None,
            task_id,
            task_json,
        )
        .await;

        // Await the synchronous reply (60s timeout — covers ONNX inference).
        match tokio::time::timeout(std::time::Duration::from_secs(60), rx).await {
            Ok(Ok(Ok(result))) => IpcResponse::Standard {
                ok: true,
                code: "OK".into(),
                message: format!("capability '{}' completed", request.task),
                corr_id: task_id.to_string(),
                data: Some(result),
            },
            Ok(Ok(Err(err))) => IpcResponse::error(
                "capability_invoke",
                "PROVIDER_ERROR",
                &format!("capability '{}' failed: {}", request.task, err),
            ),
            Ok(Err(_)) => {
                // Sender dropped without sending — model-controller disconnected.
                let mut guard = pending_capability_calls.lock().await;
                guard.remove(&task_id.to_string());
                IpcResponse::error(
                    "capability_invoke",
                    "PROVIDER_DISCONNECTED",
                    &format!(
                        "model-controller for '{}' disconnected mid-flight",
                        request.task
                    ),
                )
            }
            Err(_) => {
                // Timeout: clean up the pending entry.
                let mut guard = pending_capability_calls.lock().await;
                guard.remove(&task_id.to_string());
                IpcResponse::error(
                    "capability_invoke",
                    "TIMEOUT",
                    &format!("capability '{}' timed out after 60s", request.task),
                )
            }
        }
    }

    /// Handles a capability result arriving at `GOLGI_SINK_ROLE`.
    ///
    /// Correlates via `session_id:turn_id`, merges the capability output into the
    /// original intercepted task JSON, then delivers to the original target.
    pub(super) async fn handle_golgi_capability_response(
        pending_pipelines: &PendingPipelineRegistry,
        inboxes: &InboxRegistry,
        local_node_id: &str,
        task_json: &str,
    ) {
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(task_json) else {
            warn!("Golgi: received non-JSON response at hotel:golgi sink");
            return;
        };

        let session_id = payload
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let turn_id = payload
            .get("turn_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let corr_key = format!("{}:{}", session_id, turn_id);

        let pending = {
            let mut guard = pending_pipelines.lock().await;
            guard.remove(&corr_key)
        };

        let Some(mut pending) = pending else {
            warn!(
                "Golgi: no pending pipeline for correlation key '{}' — capability response dropped",
                corr_key
            );
            return;
        };

        // Detect capability error reply — abort the pipeline and deliver original task.
        let stage_ok = payload
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true)
            && payload.get("error").is_none();
        let elapsed_ms = unix_ts()
            .saturating_sub(pending.stage_dispatched_at)
            .saturating_mul(1000);

        // Build provenance record for this stage (enriches golgi_stages array).
        let stage_record = serde_json::json!({
            "capability": pending.current_stage_capability,
            "stage": pending.stage_index,
            "ok": stage_ok,
            "elapsed_ms": elapsed_ms,
            "output": serde_json::from_str::<serde_json::Value>(task_json)
                .unwrap_or(serde_json::Value::Null),
        });

        if !stage_ok {
            warn!(
                "Golgi: pipeline rule '{}' stage {} returned error — aborting, delivering \
                 original task {} to '{}' (on_failure passthrough)",
                pending.rule_id,
                pending.stage_index,
                pending.original_task_id,
                pending.original_target_role,
            );
            let failure_json =
                Self::inject_on_failure_marker(&pending.original_task_json, &stage_record);
            Self::deliver_inbound_task(
                inboxes,
                local_node_id,
                &pending.original_target_role,
                pending.original_target_guest_id.as_deref(),
                pending.original_task_id,
                failure_json,
            )
            .await;
            return;
        }

        // Success: accumulate provenance and merge output into current task JSON.
        pending.completed_stages.push(stage_record);
        let merged_json = Self::merge_golgi_stage_output(
            &pending.current_task_json,
            task_json,
            &pending.completed_stages,
        )
        .unwrap_or_else(|| pending.current_task_json.clone());
        pending.current_task_json = merged_json.clone();

        if let Some(next_capability) = pending.remaining_stages.pop_front() {
            // More cisternae remain — dispatch to the next stage.
            info!(
                "Golgi: pipeline rule '{}' stage {} complete — dispatching to '{}'",
                pending.rule_id, pending.stage_index, next_capability,
            );
            pending.stage_index += 1;
            pending.current_stage_capability = next_capability.clone();
            pending.stage_dispatched_at = unix_ts();

            // Rewrite reply_role/reply_to so the next capability also replies to hotel:golgi.
            let next_task_json = if let Ok(mut v) =
                serde_json::from_str::<serde_json::Value>(&pending.current_task_json)
            {
                if let Some(obj) = v.as_object_mut() {
                    obj.insert(
                        "reply_role".into(),
                        serde_json::Value::String(GOLGI_SINK_ROLE.into()),
                    );
                    obj.insert(
                        "reply_to".into(),
                        serde_json::Value::String(local_node_id.into()),
                    );
                }
                serde_json::to_string(&v).unwrap_or_else(|_| pending.current_task_json.clone())
            } else {
                pending.current_task_json.clone()
            };

            // Re-insert with updated state; TTL budget is shared across all stages.
            {
                let mut guard = pending_pipelines.lock().await;
                guard.insert(corr_key, pending);
            }

            let next_task_id = Uuid::new_v4();
            Self::deliver_inbound_task(
                inboxes,
                local_node_id,
                &next_capability,
                None,
                next_task_id,
                next_task_json,
            )
            .await;
        } else {
            // All cisternae complete — deliver final merged task to original target.
            info!(
                "Golgi: pipeline rule '{}' all {} stage(s) complete — delivering task {} to '{}'",
                pending.rule_id,
                pending.completed_stages.len(),
                pending.original_task_id,
                pending.original_target_role,
            );
            Self::deliver_inbound_task(
                inboxes,
                local_node_id,
                &pending.original_target_role,
                pending.original_target_guest_id.as_deref(),
                pending.original_task_id,
                merged_json,
            )
            .await;
        }
    }

    /// Injects `golgi_on_failure: true` and the failed stage record into the passthrough JSON.
    fn inject_on_failure_marker(task_json: &str, stage_record: &serde_json::Value) -> String {
        let Ok(mut v) = serde_json::from_str::<serde_json::Value>(task_json) else {
            return task_json.to_string();
        };
        if let Some(obj) = v.as_object_mut() {
            obj.insert("golgi_on_failure".into(), serde_json::Value::Bool(true));
            obj.insert("golgi_failed_stage".into(), stage_record.clone());
        }
        serde_json::to_string(&v).unwrap_or_else(|_| task_json.to_string())
    }

    /// Merges a capability stage's output into the current task JSON.
    ///
    /// Writes `golgi_stages` (array of all completed stage outputs) for provenance,
    /// and promotes `content` to `transcript` from the latest stage output (voice.transcribe).
    fn merge_golgi_stage_output(
        current_json: &str,
        cap_output_json: &str,
        all_completed_stages: &[serde_json::Value],
    ) -> Option<String> {
        let mut current: serde_json::Value = serde_json::from_str(current_json).ok()?;
        let cap_output: serde_json::Value = serde_json::from_str(cap_output_json).ok()?;
        if let Some(obj) = current.as_object_mut() {
            if let Some(content) = cap_output
                .get("content")
                .and_then(serde_json::Value::as_str)
            {
                obj.insert(
                    "transcript".into(),
                    serde_json::Value::String(content.to_string()),
                );
            }
            obj.insert(
                "golgi_stages".into(),
                serde_json::Value::Array(all_completed_stages.to_vec()),
            );
        }
        serde_json::to_string(&current).ok()
    }

    /// Sweeps `pending_pipelines` for entries older than [`GOLGI_PIPELINE_TTL_SECS`].
    ///
    /// For each expired entry the original task is delivered to the original target
    /// (`on_failure` passthrough), ensuring the agent is never permanently blocked
    /// by a capability that never replied.
    pub(super) async fn golgi_pipeline_watchdog(
        pending_pipelines: &PendingPipelineRegistry,
        inboxes: &InboxRegistry,
        local_node_id: &str,
    ) {
        const GOLGI_PIPELINE_TTL_SECS: u64 = 120;

        let now = unix_ts();
        let expired: Vec<(String, PendingPipeline)> = {
            let mut guard = pending_pipelines.lock().await;
            let keys: Vec<String> = guard
                .iter()
                .filter(|(_, p)| now.saturating_sub(p.created_at) >= GOLGI_PIPELINE_TTL_SECS)
                .map(|(k, _)| k.clone())
                .collect();
            keys.into_iter()
                .filter_map(|k| guard.remove(&k).map(|p| (k, p)))
                .collect()
        };

        for (corr_key, pending) in expired {
            warn!(
                "Golgi: pipeline '{}' (corr_key='{}') timed out after {}s at stage {} — \
                 delivering original task {} to '{}' (on_failure passthrough)",
                pending.rule_id,
                corr_key,
                GOLGI_PIPELINE_TTL_SECS,
                pending.stage_index,
                pending.original_task_id,
                pending.original_target_role,
            );
            let timeout_record = serde_json::json!({
                "stage": pending.stage_index,
                "ok": false,
                "reason": "ttl_timeout",
            });
            let failure_json =
                Self::inject_on_failure_marker(&pending.original_task_json, &timeout_record);
            Self::deliver_inbound_task(
                inboxes,
                local_node_id,
                &pending.original_target_role,
                pending.original_target_guest_id.as_deref(),
                pending.original_task_id,
                failure_json,
            )
            .await;
        }
    }
}
