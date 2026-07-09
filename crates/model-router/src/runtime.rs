use crate::controller::{
    BackoffStrategy, ControllerResponseEnvelope, ControllerTask, ModelProvider, NativeLiveProvider,
    NativeLiveRegistry, ProviderConfigs, ProviderOutput, ProviderRegistry, TaskKind,
    refresh_gemini_credential_pool,
};
use crate::credential_pool::{CredentialPool, RotationTrigger};
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::router_trace::{
    RouterTraceStorage, RouterTrainingRecord, SqliteRouterTraceStorage,
};
use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
use anyhow::Result;
use philotic_client::{
    GuestIdentity, IpcRequest, IpcResponse, PhiloticClient, ReturnRoute, TaskErrorPayload,
    is_ipc_disconnect,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};
use ulid::Ulid;

fn local_node_id() -> String {
    std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string())
}

type ProviderFactory =
    dyn Fn(reqwest::Client, &ProviderConfigs) -> Vec<Arc<dyn ModelProvider>> + Send + Sync;
type NativeLiveProviderFactory =
    dyn Fn(reqwest::Client, &ProviderConfigs) -> Vec<Arc<dyn NativeLiveProvider>> + Send + Sync;

pub struct ControllerGuestConfig {
    pub guest_id: &'static str,
    pub role: &'static str,
    /// Transitional knob from the earlier inline-audio prototype. Canonical audio delivery is
    /// now handled through the normal model-response artifact path, so this flag is ignored.
    pub allow_inline_audio: bool,
    pub providers: Box<ProviderFactory>,
    pub live_providers: Box<NativeLiveProviderFactory>,
}

#[derive(Debug, Clone)]
struct ReplyRoute {
    return_route: ReturnRoute,
    final_reply_to: String,
    final_reply_role: String,
    final_reply_guest_id: Option<String>,
    session_id: String,
    turn_id: String,
    chat_id: String,
}

#[derive(Debug, Clone)]
enum StubResponse {
    Text(String),
    Structured(Value),
}

pub async fn run_model_controller(config: ControllerGuestConfig) -> Result<()> {
    let _ = tracing_subscriber::fmt().try_init();
    info!(
        "Starting Materialized Model Controller Guest [{}] for role [{}]...",
        config.guest_id, config.role
    );

    let identity = GuestIdentity {
        guest_id: config.guest_id.into(),
        role: config.role.into(),
        supported_tools: Vec::new(),
    };

    let mut ipc_client = PhiloticClient::connect(identity).await?;
    ipc_client
        .send_request(IpcRequest::SubscribeInbox {
            role: config.role.into(),
        })
        .await?;
    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let stub_response = std::env::var("PHILOTIC_MODEL_ROUTER_STUB_RESPONSE").ok();

    // Open the router training-tap trace store (always-on; path from env or default).
    let trace_store: Option<Arc<dyn RouterTraceStorage>> = {
        let path = std::env::var("PHILOTIC_ROUTER_TRACE_DB").unwrap_or_else(|_| {
            let profile =
                std::env::var("PHILOTIC_PROFILE").unwrap_or_else(|_| "default".to_string());
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            format!("{home}/.philotic/{profile}/router_traces.db")
        });
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match SqliteRouterTraceStorage::open(&path) {
            Ok(store) => {
                info!(path = %path, "router training-tap trace store opened");
                Some(Arc::new(store))
            }
            Err(e) => {
                warn!(path = %path, "failed to open router trace store: {e}");
                None
            }
        }
    };

    // Open a read-write handle to the context graph for model profile observability.
    // This is the same DB the hotel daemon owns; model-router opens a sidecar connection
    // (SQLite WAL allows concurrent writers) to persist latency/error signals.
    let graph_domain: Option<GraphDomain> = {
        let path = std::env::var("PHILOTIC_GRAPH_DB_PATH").ok();
        path.and_then(|p| {
            SqliteGraphStorage::open(&p)
                .map(|s| GraphDomain::new(Arc::new(s.adapter())))
                .map_err(|e| {
                    warn!("model-router: failed to open graph domain for model profiles: {e}");
                    e
                })
                .ok()
        })
    };

    info!(
        "Listening for inbound model tasks on role [{}] from the Philotic Web...",
        config.role
    );

    // Credential pool state persists across tasks (cooldowns, pinned member)
    // even though provider configs and providers are rebuilt per task.
    // Gemini-only in slice 0 of the Model Failover Layers proposal.
    let mut gemini_pool = CredentialPool::new("gemini");
    let gemini_pool_enabled = matches!(config.role, "model" | "model.gemini");

    loop {
        match tokio::time::timeout(Duration::from_secs(5), ipc_client.recv_task()).await {
            Ok(Ok(IpcResponse::InboundTask {
                source_node,
                task_id,
                task_json,
            })) => {
                info!(
                    "Model controller [{}] received task [{}] from [{}]",
                    config.guest_id, task_id, source_node
                );

                let task_value = match serde_json::from_str::<Value>(&task_json) {
                    Ok(task) => task,
                    Err(err) => {
                        warn!("Failed to parse inbound task JSON: {}", err);
                        continue;
                    }
                };

                let reply = ReplyRoute::from_task(&task_value);

                if let Some(stub_response) =
                    short_circuit_response(&task_value, stub_response.as_deref())
                {
                    emit_stub_response(&mut ipc_client, &reply, &task_value, stub_response).await?;
                    continue;
                }

                let controller_task = match ControllerTask::from_value(&task_value) {
                    Ok(task) => task,
                    Err(err) => {
                        emit_failure(
                            &mut ipc_client,
                            &reply,
                            None,
                            None,
                            config.guest_id,
                            format!("Model controller could not interpret task: {}", err),
                        )
                        .await?;
                        continue;
                    }
                };

                let mut provider_configs = match ProviderConfigs::load(&mut ipc_client).await {
                    Ok(configs) => configs,
                    Err(err) => {
                        emit_failure(
                            &mut ipc_client,
                            &reply,
                            Some(controller_task.kind.as_str()),
                            None,
                            config.guest_id,
                            format!(
                                "Model controller failed to refresh provider config: {}",
                                err
                            ),
                        )
                        .await?;
                        continue;
                    }
                };
                if gemini_pool_enabled {
                    if let Err(err) = refresh_gemini_credential_pool(
                        &mut ipc_client,
                        &mut gemini_pool,
                        &mut provider_configs,
                    )
                    .await
                    {
                        warn!("gemini credential pool refresh failed: {err}");
                    }
                }
                let gemini_active_member = gemini_pool.active_member().map(|(idx, _)| idx);
                let providers = ProviderRegistry::new((config.providers)(
                    http_client.clone(),
                    &provider_configs,
                ));
                let live_providers = NativeLiveRegistry::new((config.live_providers)(
                    http_client.clone(),
                    &provider_configs,
                ));

                let dispatch_start = Instant::now();
                let task_kind = controller_task.kind.as_str().to_string();
                if controller_task.kind.is_native_live() {
                    let provider = match live_providers.resolve(&controller_task) {
                        Ok(provider) => provider,
                        Err(err) => {
                            emit_failure(
                                &mut ipc_client,
                                &reply,
                                Some(controller_task.kind.as_str()),
                                None,
                                config.guest_id,
                                format!("No native-live provider available for task: {}", err),
                            )
                            .await?;
                            continue;
                        }
                    };

                    info!(
                        "Dispatching {} task from role [{}] to native-live provider [{}]",
                        controller_task.kind.as_str(),
                        config.role,
                        provider.id()
                    );

                    let provider_id = provider.id().to_string();
                    match provider.invoke_live(&controller_task).await {
                        Ok(output) => {
                            let latency_ms = dispatch_start.elapsed().as_millis() as u64;
                            let native_live_model_result =
                                native_live_tool_call_model_result(&output);
                            let live_model_id = extract_output_model_gen(&output.final_output)
                                .or_else(|| controller_task.model.clone());
                            match output.final_output {
                                ProviderOutput::ToolCall {
                                    tool_name,
                                    arguments,
                                } => {
                                    record_routing_trace(
                                        trace_store.as_deref(),
                                        &reply,
                                        &provider_id,
                                        &task_kind,
                                        "tool_call",
                                        None,
                                        latency_ms,
                                        live_model_id,
                                        None,
                                    );
                                    emit_tool_call_response(
                                        &mut ipc_client,
                                        &reply,
                                        tool_name,
                                        arguments,
                                        native_live_model_result,
                                    )
                                    .await?;
                                }
                                output => {
                                    record_routing_trace(
                                        trace_store.as_deref(),
                                        &reply,
                                        &provider_id,
                                        &task_kind,
                                        "success",
                                        None,
                                        latency_ms,
                                        live_model_id,
                                        None,
                                    );
                                    let response = ControllerResponseEnvelope::from_output(
                                        &controller_task,
                                        provider.id(),
                                        output,
                                    )?;
                                    emit_text_response(&mut ipc_client, &reply, response).await?;
                                }
                            }
                        }
                        Err(err) => {
                            let latency_ms = dispatch_start.elapsed().as_millis() as u64;
                            let failure_code = classify_provider_failure(
                                Some(task_kind.as_str()),
                                Some(provider_id.as_str()),
                                &err.to_string(),
                            )
                            .code;
                            record_routing_trace(
                                trace_store.as_deref(),
                                &reply,
                                &provider_id,
                                &task_kind,
                                "failure",
                                failure_code.as_deref(),
                                latency_ms,
                                None,
                                None,
                            );
                            error!("Native-live provider invocation failed: {}", err);
                            emit_failure(
                                &mut ipc_client,
                                &reply,
                                Some(controller_task.kind.as_str()),
                                Some(provider.id()),
                                config.guest_id,
                                format!("Native-live provider invocation failed: {}", err),
                            )
                            .await?;
                        }
                    }
                } else {
                    let primary_provider = match providers.resolve(&controller_task) {
                        Ok(provider) => provider,
                        Err(err) => {
                            // No provider for this task kind. Emit an immediate failure so the
                            // session is not left hanging waiting for a response that will never
                            // come. If multiple controllers share the role inbox, the philote will
                            // take the first successful response and ignore subsequent failures.
                            warn!(
                                "Controller [{}] has no provider for {} task — emitting failure: {}",
                                config.guest_id,
                                controller_task.kind.as_str(),
                                err
                            );
                            emit_failure(
                                &mut ipc_client,
                                &reply,
                                Some(controller_task.kind.as_str()),
                                None,
                                config.guest_id,
                                format!("no_provider: {}", err),
                            )
                            .await?;
                            continue;
                        }
                    };

                    // ── Model routing reflex ──────────────────────────────────
                    // If the resolved provider's operational profile shows degraded
                    // health, walk the full supporting-provider list and substitute
                    // the first healthy alternative. Only applies when there is no
                    // explicit provider_hint — hints are honoured unconditionally.
                    let mut provider = if controller_task.provider_hint().is_none() {
                        let primary_degraded = graph_domain
                            .as_ref()
                            .and_then(|gd| {
                                gd.get_model_profile(primary_provider.id(), &local_node_id())
                                    .ok()
                                    .flatten()
                            })
                            .map(|p| p.status == "degraded")
                            .unwrap_or(false);

                        if primary_degraded {
                            let candidates = providers.all_supporting(&controller_task);
                            let healthy = candidates.into_iter().find(|p| {
                                graph_domain
                                    .as_ref()
                                    .and_then(|gd| {
                                        gd.get_model_profile(p.id(), &local_node_id())
                                            .ok()
                                            .flatten()
                                    })
                                    .map(|prof| prof.status != "degraded")
                                    .unwrap_or(true) // unknown profile = assume healthy
                            });

                            if let Some(alt) = healthy {
                                info!(
                                    "Routing reflex: [{}] degraded, substituting [{}]",
                                    primary_provider.id(),
                                    alt.id()
                                );
                                emit_falling_back(
                                    &mut ipc_client,
                                    &reply,
                                    primary_provider.id(),
                                    alt.id(),
                                )
                                .await;
                                alt
                            } else {
                                // All providers degraded — use primary, it may recover.
                                warn!(
                                    "Routing reflex: all providers degraded for {}; using primary [{}]",
                                    controller_task.kind.as_str(),
                                    primary_provider.id()
                                );
                                primary_provider
                            }
                        } else {
                            primary_provider
                        }
                    } else {
                        primary_provider
                    };

                    info!(
                        "Dispatching {} task from role [{}] to provider [{}]",
                        controller_task.kind.as_str(),
                        config.role,
                        provider.id()
                    );

                    let provider_id = provider.id().to_string();

                    // ── Dispatch with policy-driven retry ────────────────────
                    // Each provider declares AttemptPolicy (per-attempt wall-clock cap)
                    // and RetryPolicy (max attempts + backoff).  The runtime enforces the
                    // outer timeout and drives the retry loop so providers don't need to
                    // implement this themselves.
                    //
                    // Invariant: attempt_policy.total_secs × retry_policy.max_attempts < 300s
                    // (philote WaitingModel watchdog). If the controller resolves within budget,
                    // philote escalates via its own retry path; if the IPC connection drops
                    // silently, the watchdog fires and escalates to the next fallback tier.
                    let provider_result = {
                        let retry = provider.retry_policy();
                        let attempt_secs = provider.attempt_policy().total_secs;
                        let mut last_err = anyhow::anyhow!("dispatch: no attempts completed");
                        let mut result: Option<Result<ProviderOutput>> = None;
                        let mut attempt: u8 = 0;
                        // Layer 1 (credential pools): on auth/rate-limit failures the
                        // pool rotates to a sibling key and the rotated member gets
                        // exactly one fresh attempt. Bounded so worst-case wall clock
                        // (attempts + rotations) stays under the philote watchdog.
                        let mut active_pool_member = gemini_active_member;
                        let mut rotations_left: u8 = 2;

                        while attempt < retry.max_attempts {
                            if attempt > 0 {
                                warn!(
                                    "Provider [{}] retrying (attempt {}/{})",
                                    provider.id(),
                                    attempt + 1,
                                    retry.max_attempts
                                );
                                emit_dispatch_status(&mut ipc_client, &reply, attempt, "retrying")
                                    .await;
                                if let BackoffStrategy::Linear { step_ms } = retry.backoff {
                                    tokio::time::sleep(Duration::from_millis(
                                        step_ms * u64::from(attempt),
                                    ))
                                    .await;
                                }
                            }

                            // Streaming path: spawn a forwarder task for this attempt.
                            // Connect the stream IPC BEFORE starting the SSE fetch so the
                            // forwarder is ready to drain tokens immediately on arrival.
                            let attempt_result = if provider.supports_streaming(&controller_task) {
                                let (token_tx, mut token_rx) =
                                    tokio::sync::mpsc::channel::<String>(128);
                                let stream_identity = GuestIdentity {
                                    guest_id: format!("model-stream-{}", Ulid::new()),
                                    role: config.guest_id.to_string(),
                                    supported_tools: Vec::new(),
                                };
                                // 5s timeout on the stream IPC connect: if aiua is busy
                                // during a retry attempt (e.g. second attempt after first
                                // timed out), connect could hang indefinitely because it
                                // precedes the attempt_secs outer timeout on invoke_streaming.
                                let stream_ipc_opt = tokio::time::timeout(
                                    Duration::from_secs(5),
                                    PhiloticClient::connect(stream_identity),
                                )
                                .await
                                .ok()
                                .and_then(|r| r.ok());
                                let reply_clone = reply.clone();
                                tokio::spawn(async move {
                                    let Some(mut stream_ipc) = stream_ipc_opt else {
                                        return;
                                    };
                                    while let Some(token) = token_rx.recv().await {
                                        if token.is_empty() {
                                            continue;
                                        }
                                        let task_json = serde_json::to_string(&json!({
                                            "action": "streaming_token",
                                            "return_route": reply_clone.return_route.as_json(),
                                            "reply_guest_id": reply_clone.return_route.guest_id,
                                            "session_id": reply_clone.session_id,
                                            "turn_id": reply_clone.turn_id,
                                            "chat_id": reply_clone.chat_id,
                                            "content": token,
                                        }))
                                        .unwrap_or_default();
                                        // 10s timeout on the ACK from aiua: if the hotel is
                                        // slow to respond, drop the connection rather than
                                        // filling the channel and stalling invoke_streaming.
                                        let send_result = tokio::time::timeout(
                                            Duration::from_secs(10),
                                            stream_ipc.send_request(IpcRequest::EmitTask {
                                                target_node: reply_clone.return_route.node.clone(),
                                                target_role: reply_clone.return_route.role.clone(),
                                                target_guest_id: reply_clone
                                                    .return_route
                                                    .guest_id
                                                    .clone(),
                                                task_json,
                                            }),
                                        )
                                        .await;
                                        if send_result.is_err() {
                                            // Timeout or IPC error — stop forwarding tokens.
                                            // Dropping token_rx makes future send() calls in
                                            // invoke_streaming return Err immediately (no block).
                                            warn!(
                                                "streaming forwarder: send_request timeout, dropping stream"
                                            );
                                            break;
                                        }
                                    }
                                });
                                tokio::time::timeout(
                                    Duration::from_secs(attempt_secs),
                                    provider.invoke_streaming(&controller_task, token_tx),
                                )
                                .await
                                .unwrap_or_else(|_| {
                                    Err(anyhow::anyhow!(
                                        "streaming_timeout: attempt exceeded {}s budget",
                                        attempt_secs
                                    ))
                                })
                            } else {
                                tokio::time::timeout(
                                    Duration::from_secs(attempt_secs),
                                    provider.invoke(&controller_task),
                                )
                                .await
                                .unwrap_or_else(|_| {
                                    Err(anyhow::anyhow!(
                                        "streaming_timeout: attempt exceeded {}s budget",
                                        attempt_secs
                                    ))
                                })
                            };

                            match attempt_result {
                                Ok(output) => {
                                    if provider.id() == "gemini" {
                                        if let Some(idx) = active_pool_member {
                                            gemini_pool.note_success(idx);
                                        }
                                    }
                                    result = Some(Ok(output));
                                    break;
                                }
                                Err(e) => {
                                    let classified = classify_provider_failure(
                                        Some(task_kind.as_str()),
                                        Some(provider.id()),
                                        &e.to_string(),
                                    );
                                    // Layer 1: rotate to a sibling API key on auth/rate-limit
                                    // failures before the error surfaces as tier-worthy.
                                    let rotation = RotationTrigger::from_sub_kind(
                                        classified.sub_kind.as_deref(),
                                    )
                                    .filter(|_| provider.id() == "gemini" && rotations_left > 0)
                                    .and_then(|trigger| {
                                        let idx = active_pool_member?;
                                        gemini_pool
                                            .rotate_on_failure(idx, trigger)
                                            .map(|next| (trigger, idx, next))
                                    });
                                    if let Some((trigger, failed_idx, (next_idx, next_key))) =
                                        rotation
                                    {
                                        provider_configs.set_provider_api_key("gemini", next_key);
                                        let rebuilt = ProviderRegistry::new((config.providers)(
                                            http_client.clone(),
                                            &provider_configs,
                                        ));
                                        if let Some(next_provider) = rebuilt
                                            .all_supporting(&controller_task)
                                            .into_iter()
                                            .find(|p| p.id() == "gemini")
                                        {
                                            warn!(
                                                "Gemini credential pool: member {} failed ({:?}); rotating to member {}",
                                                failed_idx, trigger, next_idx
                                            );
                                            rotations_left -= 1;
                                            active_pool_member = Some(next_idx);
                                            provider = next_provider;
                                            last_err = e;
                                            attempt = retry.max_attempts.saturating_sub(1);
                                            continue;
                                        }
                                        // Rebuild lost the provider (should not happen) —
                                        // fall through to normal failure handling.
                                    }
                                    let retryable = classified.retryable.unwrap_or(false);
                                    let has_more = attempt + 1 < retry.max_attempts;
                                    if retryable && has_more {
                                        warn!(
                                            "Provider [{}] attempt {} failed (retryable, will retry): {}",
                                            provider.id(),
                                            attempt + 1,
                                            e
                                        );
                                        last_err = e;
                                    } else {
                                        result = Some(Err(e));
                                        break;
                                    }
                                }
                            }
                            attempt += 1;
                        }

                        result.unwrap_or(Err(last_err))
                    };

                    match provider_result {
                        Ok(ProviderOutput::ToolCall {
                            tool_name,
                            arguments,
                        }) => {
                            let latency_ms = dispatch_start.elapsed().as_millis() as u64;
                            record_routing_trace(
                                trace_store.as_deref(),
                                &reply,
                                &provider_id,
                                &task_kind,
                                "tool_call",
                                None,
                                latency_ms,
                                controller_task.model.clone(),
                                None,
                            );
                            emit_tool_call_response(
                                &mut ipc_client,
                                &reply,
                                tool_name,
                                arguments,
                                None,
                            )
                            .await?;
                        }
                        Ok(output) => {
                            let latency_ms = dispatch_start.elapsed().as_millis() as u64;
                            let model_id = extract_output_model_gen(&output)
                                .or_else(|| controller_task.model.clone());
                            record_routing_trace(
                                trace_store.as_deref(),
                                &reply,
                                &provider_id,
                                &task_kind,
                                "success",
                                None,
                                latency_ms,
                                model_id,
                                None,
                            );
                            if let Some(ref gd) = graph_domain {
                                if let Err(e) = gd.observe_model_outcome(
                                    &provider_id,
                                    &local_node_id(),
                                    latency_ms,
                                    true,
                                ) {
                                    warn!("observe_model_outcome (success): {e}");
                                }
                            }

                            // ── Transcription flywheel fan-out ────────────────
                            // After a successful AudioTranscribe, fire a capture
                            // envelope to role=router-listener (if enabled).
                            if controller_task.kind == TaskKind::AudioTranscribe {
                                if let ProviderOutput::Text {
                                    ref content,
                                    ref model_gen,
                                    ..
                                } = output
                                {
                                    if std::env::var("PHILOTIC_ROUTER_CAPTURE_ENABLED").as_deref()
                                        == Ok("true")
                                    {
                                        let blob_url = controller_task
                                            .media_attachments()
                                            .first()
                                            .and_then(|a| a.url.clone());

                                        let capture_json = serde_json::to_string(&json!({
                                            "kind": "transcription_capture",
                                            "session_id": reply.session_id,
                                            "turn_id": reply.turn_id,
                                            "agent_id": config.guest_id,
                                            "transcript": content,
                                            "model_gen": model_gen,
                                            "blob_download_url": blob_url,
                                            "timestamp": SystemTime::now()
                                                .duration_since(UNIX_EPOCH)
                                                .map(|d| d.as_secs())
                                                .unwrap_or(0),
                                        }))
                                        .unwrap_or_default();

                                        let fanout_identity = GuestIdentity {
                                            guest_id: format!("capture-fanout-{}", Ulid::new()),
                                            role: config.guest_id.to_string(),
                                            supported_tools: Vec::new(),
                                        };
                                        tokio::spawn(async move {
                                            let connect = tokio::time::timeout(
                                                Duration::from_secs(5),
                                                PhiloticClient::connect(fanout_identity),
                                            )
                                            .await;
                                            if let Ok(Ok(mut fanout_ipc)) = connect {
                                                let _ = tokio::time::timeout(
                                                    Duration::from_secs(10),
                                                    fanout_ipc.send_request(IpcRequest::EmitTask {
                                                        target_node: local_node_id(),
                                                        target_role: "router-listener".to_string(),
                                                        target_guest_id: None,
                                                        task_json: capture_json,
                                                    }),
                                                )
                                                .await;
                                            }
                                        });
                                    }
                                }
                            }

                            let response = ControllerResponseEnvelope::from_output(
                                &controller_task,
                                provider.id(),
                                output,
                            )?;
                            emit_text_response(&mut ipc_client, &reply, response).await?;
                        }
                        Err(err) => {
                            let latency_ms = dispatch_start.elapsed().as_millis() as u64;
                            let failure_code = classify_provider_failure(
                                Some(task_kind.as_str()),
                                Some(provider_id.as_str()),
                                &err.to_string(),
                            )
                            .code;
                            record_routing_trace(
                                trace_store.as_deref(),
                                &reply,
                                &provider_id,
                                &task_kind,
                                "failure",
                                failure_code.as_deref(),
                                latency_ms,
                                None,
                                None,
                            );
                            if let Some(ref gd) = graph_domain {
                                if let Err(e) = gd.observe_model_outcome(
                                    &provider_id,
                                    &local_node_id(),
                                    latency_ms,
                                    false,
                                ) {
                                    warn!("observe_model_outcome (failure): {e}");
                                }
                            }
                            error!("Provider invocation failed: {}", err);
                            emit_failure(
                                &mut ipc_client,
                                &reply,
                                Some(controller_task.kind.as_str()),
                                Some(provider.id()),
                                config.guest_id,
                                format!("Provider invocation failed: {}", err),
                            )
                            .await?;
                        }
                    }
                }
            }
            Ok(Ok(other)) => {
                info!(
                    "Model controller [{}] received non-task IPC message: {:?}",
                    config.guest_id, other
                );
            }
            Ok(Err(err)) => {
                if is_ipc_disconnect(&err) {
                    info!(
                        "Hotel IPC disconnected; model controller [{}] exiting.",
                        config.guest_id
                    );
                    return Ok(());
                }
                warn!("IPC recv error: {}", err);
            }
            Err(_) => {}
        }
    }
}

fn short_circuit_response(task: &Value, stub_response: Option<&str>) -> Option<StubResponse> {
    let stub = stub_response?;

    if task.get("prompt").and_then(Value::as_str).is_some() {
        if stub.contains('=') {
            let turn_id = task["context_projection"]["conversation_turn"]["conversation_turn_id"]
                .as_str()
                .unwrap_or_else(|| {
                    // Fallback to older path if needed
                    task["context_projection"]["current_turn"]["id"]
                        .as_str()
                        .unwrap_or("")
                });
            let iteration = task["context_projection"]["active_step"]["iteration"]
                .as_u64()
                .unwrap_or_else(|| {
                    // Fallback to older path if needed
                    task["context_projection"]["cognitive_step"]["iteration"]
                        .as_u64()
                        .unwrap_or(0)
                });

            let mut turn_match = None;
            for pair in stub.split(';') {
                if let Some((k, v)) = pair.split_once('=') {
                    // Try exact match with iteration (e.g. "turn-1:1")
                    if iteration > 0 {
                        let iter_key = format!("{}:{}", turn_id, iteration);
                        if k == iter_key {
                            info!(
                                "Model controller turn/iteration-aware stub mode returning response for [{}].",
                                iter_key
                            );
                            return Some(parse_stub_response(v));
                        }
                    }

                    // Keep track of plain turn_id match as fallback
                    if k == turn_id {
                        turn_match = Some(parse_stub_response(v));
                    }
                }
            }
            if let Some(v) = turn_match {
                info!(
                    "Model controller turn-aware stub mode returning response for [{}].",
                    turn_id
                );
                return Some(v);
            }
        }

        info!("Model controller stub mode returning deterministic response.");
        return Some(parse_stub_response(stub));
    }

    None
}

fn parse_stub_response(raw: &str) -> StubResponse {
    let trimmed = raw.trim();
    if let Some(json_text) = trimmed.strip_prefix("json:") {
        let value: Value =
            serde_json::from_str(json_text).unwrap_or_else(|_| json!({ "display_text": trimmed }));
        return StubResponse::Structured(value);
    }
    StubResponse::Text(trimmed.to_string())
}

async fn emit_stub_response(
    ipc_client: &mut PhiloticClient,
    reply: &ReplyRoute,
    task_value: &Value,
    stub_response: StubResponse,
) -> Result<()> {
    match stub_response {
        StubResponse::Text(response_text) => {
            emit_text_response(
                ipc_client,
                reply,
                ControllerResponseEnvelope {
                    capability: TaskKind::TextGenerate.as_str().to_string(),
                    content: response_text.clone(),
                    result: json!({ "display_text": response_text }),
                    artifacts: Vec::new(),
                    trace: Default::default(),
                    provider_output: Value::Null,
                },
            )
            .await
        }
        StubResponse::Structured(value) => {
            validate_stub_prompt(task_value, &value)?;

            if let Some(tool_call) = value.get("tool_call").and_then(Value::as_object) {
                let tool_name = tool_call
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or("echo")
                    .to_string();
                let arguments = tool_call
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let model_result = json!({
                    "capability": TaskKind::TextGenerate.as_str(),
                    "result": {
                        "active_plan": value.get("active_plan").cloned(),
                        "spoken_text": value.get("spoken_text").cloned(),
                        "memory_concept": value.get("memory_concept").cloned(),
                    },
                    "artifacts": [],
                    "trace": {},
                    "provider_output": Value::Null,
                });
                emit_tool_call_response(ipc_client, reply, tool_name, arguments, Some(model_result))
                    .await
            } else {
                let display_text = value
                    .get("display_text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let content = value
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| display_text.clone());
                emit_text_response(
                    ipc_client,
                    reply,
                    ControllerResponseEnvelope {
                        capability: TaskKind::TextGenerate.as_str().to_string(),
                        content,
                        result: json!({
                            "display_text": display_text,
                            "spoken_text": value.get("spoken_text").cloned(),
                            "memory_concept": value.get("memory_concept").cloned(),
                            "active_plan": value.get("active_plan").cloned(),
                        }),
                        artifacts: Vec::new(),
                        trace: Default::default(),
                        provider_output: Value::Null,
                    },
                )
                .await
            }
        }
    }
}

fn validate_stub_prompt(task_value: &Value, stub_value: &Value) -> Result<()> {
    let Some(required) = stub_value
        .get("require_prompt_substrings")
        .and_then(Value::as_array)
    else {
        return Ok(());
    };

    let prompt = ControllerTask::from_value(task_value)
        .ok()
        .and_then(|task| {
            task.composed_prompt_text()
                .or_else(|| task.prompt_text().map(str::to_string))
        })
        .unwrap_or_default();
    for needle in required.iter().filter_map(Value::as_str) {
        if !prompt.contains(needle) {
            anyhow::bail!(
                "stub validation failed: prompt missing required substring {:?}",
                needle
            );
        }
    }
    Ok(())
}

async fn emit_text_response(
    ipc_client: &mut PhiloticClient,
    reply: &ReplyRoute,
    response: ControllerResponseEnvelope,
) -> Result<()> {
    let reply_req = IpcRequest::EmitTask {
        target_node: reply.return_route.node.clone(),
        target_role: reply.return_route.role.clone(),
        target_guest_id: reply.return_route.guest_id.clone(),
        task_json: json!({
            "action": "model_response",
            "agent_action": {
                "kind": "respond",
                "content": response.content,
                "model_result": {
                    "capability": response.capability,
                    "result": response.result,
                    "artifacts": response.artifacts.iter().map(|artifact| {
                        json!({
                            "kind": artifact.kind,
                            "mime_type": artifact.mime_type,
                            "output_format": artifact.output_format,
                            "payload": artifact.payload,
                        })
                    }).collect::<Vec<_>>(),
                    "trace": {
                        "provider": response.trace.provider,
                        "model": response.trace.model,
                        "voice": response.trace.voice,
                    },
                    "provider_output": response.provider_output,
                }
            },
            "return_route": reply.return_route.as_json(),
            "reply_guest_id": reply.return_route.guest_id,
            "session_id": reply.session_id,
            "turn_id": reply.turn_id,
            "chat_id": reply.chat_id,
            "content": response.content,
            "final_reply_to": reply.final_reply_to,
            "final_reply_role": reply.final_reply_role,
            "final_reply_guest_id": reply.final_reply_guest_id
        })
        .to_string(),
    };

    tokio::time::timeout(Duration::from_secs(30), ipc_client.send_request(reply_req))
        .await
        .map_err(|_| anyhow::anyhow!("emit_text_response: ipc ack timeout after 30s"))??;
    Ok(())
}

async fn emit_tool_call_response(
    ipc_client: &mut PhiloticClient,
    reply: &ReplyRoute,
    tool_name: String,
    arguments: serde_json::Value,
    model_result: Option<Value>,
) -> Result<()> {
    let reply_req = IpcRequest::EmitTask {
        target_node: reply.return_route.node.clone(),
        target_role: reply.return_route.role.clone(),
        target_guest_id: reply.return_route.guest_id.clone(),
        task_json: json!({
            "action": "model_response",
            "agent_action": {
                "kind": "tool_call",
                "tool_name": tool_name,
                "arguments": arguments,
                "model_result": model_result,
            },
            "return_route": reply.return_route.as_json(),
            "reply_guest_id": reply.return_route.guest_id,
            "session_id": reply.session_id,
            "turn_id": reply.turn_id,
            "chat_id": reply.chat_id,
            "final_reply_to": reply.final_reply_to,
            "final_reply_role": reply.final_reply_role,
            "final_reply_guest_id": reply.final_reply_guest_id
        })
        .to_string(),
    };

    tokio::time::timeout(Duration::from_secs(30), ipc_client.send_request(reply_req))
        .await
        .map_err(|_| anyhow::anyhow!("emit_tool_call_response: ipc ack timeout after 30s"))??;
    Ok(())
}

fn native_live_tool_call_model_result(
    output: &crate::controller::NativeLiveTurnOutput,
) -> Option<Value> {
    if output.session_marker.is_none() && output.pending_function_call_id.is_none() {
        return None;
    }

    Some(json!({
        "native_live": {
            "session_marker": output.session_marker.as_ref().map(|marker| {
                json!({
                    "provider_session_id": marker.provider_session_id,
                    "resumption_handle": marker.resumption_handle,
                    "protocol": marker.protocol,
                })
            }),
            "pending_function_call_id": output.pending_function_call_id,
        }
    }))
}

async fn emit_failure(
    ipc_client: &mut PhiloticClient,
    reply: &ReplyRoute,
    capability: Option<&str>,
    provider: Option<&str>,
    guest_id: &str,
    message: String,
) -> Result<()> {
    let error_payload = classify_provider_failure(capability, provider, &message);
    error!(
        "Emitting model failure capability={:?} provider={:?}: {}",
        capability, provider, message
    );
    let raw_text = format!(
        "[{}][{}] {}: {}",
        guest_id,
        capability.unwrap_or("unknown"),
        provider.unwrap_or("unknown"),
        message
    );
    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        ipc_client.send_request(IpcRequest::PushHealEntry {
            guest_id: guest_id.to_string(),
            raw_text,
        }),
    )
    .await;
    let reply_req = IpcRequest::EmitTask {
        target_node: reply.return_route.node.clone(),
        target_role: reply.return_route.role.clone(),
        target_guest_id: reply.return_route.guest_id.clone(),
        task_json: json!({
            "action": "model_response",
            "agent_action": {
                "kind": "fail",
                "message": message,
                "model_result": {
                    "capability": capability,
                    "error": serde_json::to_value(&error_payload)?,
                }
            },
            "error": serde_json::to_value(&error_payload)?,
            "return_route": reply.return_route.as_json(),
            "reply_guest_id": reply.return_route.guest_id,
            "session_id": reply.session_id,
            "turn_id": reply.turn_id,
            "chat_id": reply.chat_id,
            "content": message,
            "final_reply_to": reply.final_reply_to,
            "final_reply_role": reply.final_reply_role,
            "final_reply_guest_id": reply.final_reply_guest_id
        })
        .to_string(),
    };

    tokio::time::timeout(Duration::from_secs(30), ipc_client.send_request(reply_req))
        .await
        .map_err(|_| anyhow::anyhow!("emit_failure: ipc ack timeout after 30s"))??;
    Ok(())
}

/// Emit a model_dispatch_status event to philote when the routing reflex
/// substitutes a degraded provider with a healthy fallback.
async fn emit_falling_back(
    ipc_client: &mut PhiloticClient,
    reply: &ReplyRoute,
    from: &str,
    to: &str,
) {
    let label = format!("_(switching model: {from} \u{2192} {to})_");
    let task_json = json!({
        "action": "model_dispatch_status",
        "content": label,
        "return_route": reply.return_route.as_json(),
        "reply_guest_id": reply.return_route.guest_id,
        "session_id": reply.session_id,
        "turn_id": reply.turn_id,
        "chat_id": reply.chat_id,
    })
    .to_string();
    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        ipc_client.send_request(IpcRequest::EmitTask {
            target_node: reply.return_route.node.clone(),
            target_role: reply.return_route.role.clone(),
            target_guest_id: reply.return_route.guest_id.clone(),
            task_json,
        }),
    )
    .await;
}

/// Emit a dispatch status event to philote so it can surface transient state
/// (e.g. "(retrying...)" in the Telegram draft) without a full model_response.
/// The human-readable label is formatted here and carried in `content` so philote
/// can forward it without parsing additional fields.
async fn emit_dispatch_status(
    ipc_client: &mut PhiloticClient,
    reply: &ReplyRoute,
    attempt: u8,
    kind: &str,
) {
    let label = match kind {
        "retrying" => format!("_(retrying\u{2026} attempt {})_", attempt + 1),
        other => format!("_({other})_"),
    };
    let task_json = json!({
        "action": "model_dispatch_status",
        "content": label,
        "return_route": reply.return_route.as_json(),
        "reply_guest_id": reply.return_route.guest_id,
        "session_id": reply.session_id,
        "turn_id": reply.turn_id,
        "chat_id": reply.chat_id,
    })
    .to_string();
    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        ipc_client.send_request(IpcRequest::EmitTask {
            target_node: reply.return_route.node.clone(),
            target_role: reply.return_route.role.clone(),
            target_guest_id: reply.return_route.guest_id.clone(),
            task_json,
        }),
    )
    .await;
}

/// Extract an HTTP status code (400..=599) from a provider error message.
///
/// Providers format failures as e.g. `"Gemini API error (400): ..."` or append
/// a bracketed ` [503]` on streaming errors. Only 3-digit tokens delimited by
/// `(`/`)`/`[`/`]`/`:`/space (or string boundaries) count, so token counts like
/// "4096 tokens" never match.
fn extract_http_status(message: &str) -> Option<u16> {
    let bytes = message.as_bytes();
    let is_delim = |c: u8| matches!(c, b'(' | b')' | b'[' | b']' | b':' | b' ' | b',' | b'.');
    for i in 0..bytes.len().saturating_sub(2) {
        if !bytes[i..i + 3].iter().all(u8::is_ascii_digit) {
            continue;
        }
        let before_ok = i == 0 || is_delim(bytes[i - 1]);
        let after_ok = i + 3 == bytes.len() || is_delim(bytes[i + 3]);
        if !before_ok || !after_ok {
            continue;
        }
        if let Ok(n) = std::str::from_utf8(&bytes[i..i + 3])
            .unwrap_or("")
            .parse::<u16>()
        {
            if (400..=599).contains(&n) {
                return Some(n);
            }
        }
    }
    None
}

fn classify_provider_failure(
    capability: Option<&str>,
    provider: Option<&str>,
    message: &str,
) -> TaskErrorPayload {
    let mut payload = TaskErrorPayload::provider_failure(
        "model-router",
        capability,
        provider,
        message.to_string(),
    );
    payload.status = extract_http_status(message);

    let malformed_tool_call = message.contains("tool_call.arguments missing from")
        || message.contains("returned invalid tool_call")
        || message.contains("returned unsupported tool_call");

    if malformed_tool_call {
        payload.code = Some("MODEL_INVALID_TOOL_CALL".into());
        payload.retryable = Some(true);
        payload.sub_kind = Some("content_error".into());
        // If the same provider mangles the repaired request too, the caller
        // should switch providers rather than fail the turn.
        payload.error_class = Some("switch_provider".into());
        return payload;
    }

    // Network-level failures: connection refused, DNS, TLS, socket errors.
    let is_network = message.contains("connection refused")
        || message.contains("Connection refused")
        || message.contains("failed to connect")
        || message.contains("dns error")
        || message.contains("No such host")
        || message.contains("connection error")
        || message.contains("error sending request");

    if is_network {
        payload.sub_kind = Some("network_error".into());
        payload.retryable = Some(true);
        payload.error_class = Some("retry_same_provider".into());
        return payload;
    }

    // Streaming idle timeout — emitted by providers when the SSE stream stalls.
    if message.contains("streaming_timeout") {
        payload.sub_kind = Some("streaming_timeout".into());
        payload.retryable = Some(true);
        payload.error_class = Some("retry_same_provider".into());
        return payload;
    }

    // Rate limit (HTTP 429). Immediate same-provider retry will 429 again —
    // the next different-provider tier is the productive move.
    if message.contains("429") || message.contains("rate limit") || message.contains("quota") {
        payload.sub_kind = Some("rate_limit".into());
        payload.retryable = Some(true);
        payload.error_class = Some("switch_provider".into());
        return payload;
    }

    let lower_message = message.to_lowercase();

    // Auth/key errors (4xx) — the provider tier is misconfigured, not flaky.
    // Retrying an expired or invalid key only burns the caller's turn watchdog.
    if message.contains("401")
        || message.contains("403")
        || lower_message.contains("api key expired")
        || lower_message.contains("api key not valid")
        || lower_message.contains("invalid api key")
        || lower_message.contains("api_key_invalid")
        || lower_message.contains("unauthorized")
        || lower_message.contains("unauthenticated")
    {
        payload.sub_kind = Some("provider_auth".into());
        payload.retryable = Some(false);
        payload.error_class = Some("fatal".into());
        return payload;
    }

    // Generic provider-side HTTP error (5xx) — transient; retrying (same
    // provider or next tier) may succeed.
    if message.contains("500")
        || message.contains("502")
        || message.contains("503")
        || message.contains("504")
    {
        payload.sub_kind = Some("provider_error".into());
        payload.retryable = Some(true);
        payload.error_class = Some("retry_same_provider".into());
        return payload;
    }

    // Contract-level 4xx (400 INVALID_ARGUMENT, 404, 422, refusal…): the exact
    // same request will fail identically on this provider — switch providers.
    let is_contract_4xx = matches!(payload.status, Some(s) if (400..500).contains(&s))
        || lower_message.contains("invalid_argument")
        || lower_message.contains("invalid argument")
        || lower_message.contains("failed_precondition")
        || lower_message.contains("bad request");

    if is_contract_4xx {
        payload.sub_kind = Some("invalid_request".into());
        payload.retryable = Some(false);
        payload.error_class = Some("switch_provider".into());
        return payload;
    }

    payload
}

// ── Training-tap helper ───────────────────────────────────────────────────────

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Extract the `model_gen` string from a `ProviderOutput` without consuming it.
fn extract_output_model_gen(output: &ProviderOutput) -> Option<String> {
    match output {
        ProviderOutput::Text { model_gen, .. } => model_gen.clone(),
        ProviderOutput::Embedding { model_gen, .. } => Some(model_gen.clone()),
        ProviderOutput::Audio(_) | ProviderOutput::ToolCall { .. } => None,
    }
}

/// Record a routing decision into the training-tap store, if one is open.
///
/// Failures to write are logged as warnings and do not abort the request path.
fn record_routing_trace(
    store: Option<&dyn RouterTraceStorage>,
    reply: &ReplyRoute,
    provider_id: &str,
    task_kind: &str,
    outcome: &str,
    failure_code: Option<&str>,
    latency_ms: u64,
    model_id: Option<String>,
    token_count: Option<u64>,
) {
    let Some(store) = store else { return };
    let record = RouterTrainingRecord {
        trace_id: Ulid::new().to_string(),
        agent_id: String::new(),
        session_id: reply.session_id.clone(),
        turn_id: reply.turn_id.clone(),
        provider_id: provider_id.to_string(),
        model_id,
        task_kind: task_kind.to_string(),
        outcome: outcome.to_string(),
        failure_code: failure_code.map(str::to_string),
        latency_ms: Some(latency_ms),
        token_count,
        timestamp: now_epoch_secs(),
    };
    if let Err(e) = store.record_trace(&record) {
        warn!(provider = %provider_id, outcome = %outcome, "router trace write failed: {e}");
    }
}

impl ReplyRoute {
    fn from_task(task: &Value) -> Self {
        let local_node_id = local_node_id();
        let return_route = ReturnRoute::from_task(task, &local_node_id, "agent");
        Self {
            return_route,
            final_reply_to: task
                .get("final_reply_to")
                .and_then(Value::as_str)
                .unwrap_or(&local_node_id)
                .to_string(),
            final_reply_role: task
                .get("final_reply_role")
                .and_then(Value::as_str)
                .unwrap_or("membrane")
                .to_string(),
            final_reply_guest_id: task
                .get("final_reply_guest_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            session_id: task
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            turn_id: task
                .get("turn_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            chat_id: task
                .get("chat_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }
    }
}

#[cfg(test)]
mod failure_tests {
    use super::{classify_provider_failure, extract_http_status};

    #[test]
    fn classify_provider_failure_marks_malformed_tool_calls_retryable() {
        let payload = classify_provider_failure(
            Some("text.generate"),
            Some("gemini"),
            "Provider invocation failed: tool_call.arguments missing from Gemini response",
        );

        assert_eq!(payload.kind, "provider_failure");
        assert_eq!(payload.code.as_deref(), Some("MODEL_INVALID_TOOL_CALL"));
        assert_eq!(payload.retryable, Some(true));
        assert_eq!(payload.provider.as_deref(), Some("gemini"));
        assert_eq!(payload.capability.as_deref(), Some("text.generate"));
    }

    #[test]
    fn classify_provider_failure_leaves_generic_errors_non_retryable() {
        let payload = classify_provider_failure(
            Some("voice.synthesize"),
            Some("elevenlabs"),
            "Provider invocation failed: missing voice",
        );

        assert_eq!(payload.kind, "provider_failure");
        assert_eq!(payload.code, None);
        assert_eq!(payload.retryable, None);
    }

    #[test]
    fn classify_provider_failure_marks_expired_api_key_non_retryable() {
        let payload = classify_provider_failure(
            Some("text.generate"),
            Some("gemini"),
            "Gemini API error (400): API key expired. Please renew the API key.",
        );

        assert_eq!(payload.kind, "provider_failure");
        assert_eq!(payload.provider.as_deref(), Some("gemini"));
        assert_eq!(payload.capability.as_deref(), Some("text.generate"));
        assert_eq!(payload.sub_kind.as_deref(), Some("provider_auth"));
        assert_eq!(payload.retryable, Some(false));
        assert_eq!(payload.error_class.as_deref(), Some("fatal"));
        assert_eq!(payload.status, Some(400));
    }

    /// Forensic 2026-07-08: a Gemini 400 INVALID_ARGUMENT must carry a
    /// machine-readable switch_provider class so philote engages the fallback
    /// ladder instead of failing the turn as MODEL_EMPTY_RESPONSE.
    #[test]
    fn classify_provider_failure_marks_contract_400_switch_provider() {
        let payload = classify_provider_failure(
            Some("text.generate"),
            Some("gemini"),
            "Gemini API error (400): Request contains an invalid argument. INVALID_ARGUMENT",
        );

        assert_eq!(payload.sub_kind.as_deref(), Some("invalid_request"));
        assert_eq!(payload.retryable, Some(false));
        assert_eq!(payload.error_class.as_deref(), Some("switch_provider"));
        assert_eq!(payload.status, Some(400));
    }

    #[test]
    fn classify_provider_failure_marks_5xx_retry_same_provider() {
        let payload = classify_provider_failure(
            Some("text.generate"),
            Some("gemini"),
            "Gemini API error (503): The model is overloaded. Please try again later.",
        );

        assert_eq!(payload.sub_kind.as_deref(), Some("provider_error"));
        assert_eq!(payload.retryable, Some(true));
        assert_eq!(payload.error_class.as_deref(), Some("retry_same_provider"));
        assert_eq!(payload.status, Some(503));
    }

    #[test]
    fn classify_provider_failure_marks_rate_limit_switch_provider() {
        let payload = classify_provider_failure(
            Some("text.generate"),
            Some("gemini"),
            "Gemini API error (429): Resource has been exhausted (e.g. check quota).",
        );

        assert_eq!(payload.sub_kind.as_deref(), Some("rate_limit"));
        assert_eq!(payload.error_class.as_deref(), Some("switch_provider"));
        assert_eq!(payload.status, Some(429));
    }

    #[test]
    fn classify_provider_failure_marks_network_retry_same_provider() {
        let payload = classify_provider_failure(
            Some("text.generate"),
            Some("ollama"),
            "error sending request for url (http://127.0.0.1:11434/api/chat)",
        );

        assert_eq!(payload.sub_kind.as_deref(), Some("network_error"));
        assert_eq!(payload.error_class.as_deref(), Some("retry_same_provider"));
    }

    #[test]
    fn extract_http_status_reads_provider_error_formats() {
        assert_eq!(
            extract_http_status("Gemini API error (400): bad request"),
            Some(400)
        );
        assert_eq!(
            extract_http_status("Anthropic API error (529): overloaded"),
            Some(529)
        );
        assert_eq!(extract_http_status("stream stalled [503]"), Some(503));
        assert_eq!(extract_http_status("HTTP 404 model not found"), Some(404));
        // Token counts and non-status digits never match.
        assert_eq!(extract_http_status("prompt is 4096 tokens"), None);
        assert_eq!(extract_http_status("id 123456 rejected"), None);
        assert_eq!(extract_http_status("no digits at all"), None);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        StubResponse, native_live_tool_call_model_result, parse_stub_response,
        short_circuit_response, validate_stub_prompt,
    };
    use crate::controller::{NativeLiveSessionMarker, NativeLiveTurnOutput, ProviderOutput};
    use serde_json::json;

    #[test]
    fn parse_stub_response_supports_json_prefix() {
        let parsed = parse_stub_response(r#"json:{"display_text":"hello"}"#);
        match parsed {
            StubResponse::Structured(value) => {
                assert_eq!(value["display_text"], "hello");
            }
            other => panic!("expected structured stub response, got {other:?}"),
        }
    }

    #[test]
    fn short_circuit_response_prefers_iteration_specific_stub() {
        let task = json!({
            "prompt": "continue",
            "context_projection": {
                "conversation_turn": { "conversation_turn_id": "turn-1" },
                "active_step": { "iteration": 2 }
            }
        });

        let stub = r#"turn-1=json:{"display_text":"fallback"};turn-1:2=json:{"display_text":"iteration-two"}"#;
        let parsed = short_circuit_response(&task, Some(stub)).expect("stub should match");
        match parsed {
            StubResponse::Structured(value) => {
                assert_eq!(value["display_text"], "iteration-two");
            }
            other => panic!("expected structured stub response, got {other:?}"),
        }
    }

    #[test]
    fn validate_stub_prompt_checks_composed_reentry_prompt() {
        let task = json!({
            "kind": "text.generate",
            "context": {
                "active_turn": { "role": "user", "text": "Keep going." },
                "tool_history": [{
                    "index": 1,
                    "tool_name": "echo",
                    "arguments": { "text": "hello structured tool" },
                    "result": "hello structured tool"
                }],
                "active_plan": {
                    "goal": "echo hello structured tool",
                    "status": "in_progress",
                    "steps": [{
                        "id": 1,
                        "description": "call echo",
                        "tool_name": "echo",
                        "status": "in_progress"
                    }]
                }
            }
        });

        let stub = json!({
            "require_prompt_substrings": [
                "[Tool call history]",
                "Call 1: echo({\"text\":\"hello structured tool\"})",
                "[Active plan]",
                "Goal: echo hello structured tool"
            ]
        });

        validate_stub_prompt(&task, &stub).expect("composed prompt should satisfy stub checks");
    }

    #[test]
    fn native_live_tool_call_model_result_carries_function_call_id_and_marker() {
        let output = NativeLiveTurnOutput {
            final_output: ProviderOutput::ToolCall {
                tool_name: "session.status".into(),
                arguments: json!({}),
            },
            partial_text_deltas: Vec::new(),
            session_marker: Some(NativeLiveSessionMarker {
                provider_session_id: None,
                resumption_handle: Some("resume-123".into()),
                protocol: Some("gemini-live-v1beta".into()),
            }),
            pending_function_call_id: Some("call-1".into()),
            generation_complete: false,
            turn_complete: false,
        };

        let model_result =
            native_live_tool_call_model_result(&output).expect("metadata should be present");
        assert_eq!(
            model_result["native_live"]["pending_function_call_id"],
            json!("call-1")
        );
        assert_eq!(
            model_result["native_live"]["session_marker"]["resumption_handle"],
            json!("resume-123")
        );
    }
}
