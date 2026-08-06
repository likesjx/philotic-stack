use crate::controller::{
    BackoffStrategy, ControllerResponseEnvelope, ControllerTask, ModelProvider, NativeLiveProvider,
    NativeLiveRegistry, ProviderConfigs, ProviderOutput, ProviderRegistry, TaskKind,
    refresh_gemini_credential_pool,
};
use crate::credential_pool::{CredentialPool, RotationTrigger};
use crate::transcribe_stream::{
    self, DEFAULT_IDLE_TIMEOUT_SECS, DEFAULT_MAX_SESSIONS, ElevenLabsRealtimeConnector,
    IpcStreamReplySink, StreamFrame, StreamReplySink, SttSessionManager, parse_stream_frame,
};
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::router_trace::{
    RouterTraceStorage, RouterTrainingRecord, SqliteRouterTraceStorage,
};
use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
use anyhow::{Context, Result};
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

/// Fallback text model for the OpenRouter controller when neither
/// `PHILOTIC_OPENROUTER_DEFAULT_MODEL` nor `config:openrouter_default_model`
/// is set.
///
/// The OpenRouter controller reuses the generic OpenAI-compat provider
/// (`OpenAIProvider`), whose own hardcoded fallback is `gpt-4.1-mini` — a bare
/// slug that is NOT a valid OpenRouter model id. The controller must therefore
/// supply its OWN OpenRouter-valid default rather than inheriting the OpenAI
/// one. Keep this as the single source of truth the
/// `model-controller-openrouter` bin references so the fallback can't silently
/// regress back onto an OpenAI-shaped slug.
pub const DEFAULT_OPENROUTER_MODEL: &str = "z-ai/glm-5.2";

/// Default overall per-task provider-dispatch cap, in seconds.
///
/// Covers ANY await between receiving a task and obtaining a provider result —
/// not just the provider's own HTTP round trip. Before this cap existed, the
/// pre-dispatch IPC round trips in `ProviderConfigs::load` / credential-pool
/// refresh (roughly a dozen sequential `send_request` calls with no per-call
/// timeout) and the provider attempt+retry loop were each theoretically
/// bounded on their own, but nothing capped the SUM — a single stuck IPC round
/// trip before the provider was ever dispatched left the turn silent with the
/// provider's own streaming caps (STREAMING_CONNECT/IDLE/TOTAL_SECS in
/// gemini.rs) never engaging, riding philote's coarse 300s WaitingModel
/// watchdog instead of failing fast onto the next fallback tier.
/// (2026-07-09 stuck-turn forensic RC-1.)
///
/// Sized well under the 300s watchdog so a breach always leaves room for the
/// FailTask → ladder-escalation round trip. Env-tunable via
/// `PHILOTIC_MODEL_DISPATCH_TIMEOUT_SECS` for operators running against a
/// known-slow vault/config backend without a rebuild.
const MODEL_DISPATCH_TIMEOUT_SECS_DEFAULT: u64 = 55;

fn model_dispatch_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("PHILOTIC_MODEL_DISPATCH_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(MODEL_DISPATCH_TIMEOUT_SECS_DEFAULT),
    )
}

/// Upper bound for the scaled `voice.transcribe` dispatch budget. Kept safely
/// under the 300s WaitingModel watchdog so a breach still leaves room for the
/// FailTask → ladder-escalation round trip.
const TRANSCRIBE_MAX_BUDGET_SECS: u64 = 240;

/// Assumed clip length (seconds) when a voice memo carries no duration metadata,
/// chosen so the budget defaults to the generous cap (30 + 1.5×140 = 240) rather
/// than the short default — an unknown-length memo should get room to finish, not
/// a guaranteed cut-off.
const TRANSCRIBE_UNKNOWN_DURATION_SECS: u64 = 140;

/// Dispatch budget for a single task. `voice.transcribe` of a long voice memo
/// legitimately runs past the default 55s cap, so it gets a budget proportional to
/// the clip length: `30 + 1.5 × duration_secs`, floored at the default dispatch
/// timeout and capped at [`TRANSCRIBE_MAX_BUDGET_SECS`] (a 10s clip → floor 55s, a
/// 60s clip → 120s, a 180s clip → the 240s cap). Duration comes from the audio
/// attachment (e.g. Telegram `voice.duration`); when absent it falls back to
/// [`TRANSCRIBE_UNKNOWN_DURATION_SECS`] → the cap. Every other task kind uses the
/// default dispatch timeout unchanged.
/// Pure budget math (seconds): `30 + 1.5 × duration`, clamped to
/// `[default_secs, TRANSCRIBE_MAX_BUDGET_SECS]`. A `None` duration assumes a long
/// clip ([`TRANSCRIBE_UNKNOWN_DURATION_SECS`]) so the budget lands at the cap
/// rather than the short default.
fn transcribe_budget_secs(default_secs: u64, duration_secs: Option<u64>) -> u64 {
    let dur = duration_secs.unwrap_or(TRANSCRIBE_UNKNOWN_DURATION_SECS);
    (30 + dur.saturating_mul(3) / 2).clamp(default_secs, TRANSCRIBE_MAX_BUDGET_SECS)
}

fn effective_dispatch_timeout(task: &ControllerTask) -> Duration {
    let default = model_dispatch_timeout();
    if task.kind != TaskKind::AudioTranscribe {
        return default;
    }
    let duration_secs = task
        .context
        .attachments
        .iter()
        .filter_map(|a| a.duration_secs)
        .max();
    let budget = transcribe_budget_secs(default.as_secs(), duration_secs);
    info!(
        capability = "voice.transcribe",
        clip_duration_secs = ?duration_secs,
        budget_secs = budget,
        scaled_from_duration = duration_secs.is_some(),
        "sized transcription dispatch budget"
    );
    Duration::from_secs(budget)
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
    /// Persona/agent that owns this turn, threaded from philote's
    /// `ModelRequestPayload.agent_id`. Empty string only for legacy payloads
    /// that predate the field. Recorded verbatim into the training-tap trace.
    agent_id: String,
    /// Shadow-mode (`PHILOTIC_SHADOW_ORACLE`) annotations forwarded by philote:
    /// the oracle's top pick and whether it agreed with the ladder's resolved
    /// role. `None` when shadow mode was off. Persisted to the trace store only.
    oracle_pick: Option<String>,
    oracle_agreement: Option<bool>,
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

    // Streaming transcription sessions (voice.transcribe.stream) live at the
    // runtime level — they are long-lived WS sessions, not one-shot provider
    // invocations. Only the ElevenLabs controller receives these frames (role
    // model.elevenlabs), but the manager is harmless elsewhere.
    let mut stt_sessions = SttSessionManager::new(
        DEFAULT_MAX_SESSIONS,
        Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS),
    );

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

                // Streaming transcription frames are session-level operations
                // handled by the runtime's session manager — they must never
                // reach normal one-shot provider dispatch.
                if transcribe_stream::is_stream_task(&task_value) {
                    handle_stream_frame(
                        &mut ipc_client,
                        &mut stt_sessions,
                        &task_value,
                        config.guest_id,
                    )
                    .await;
                    continue;
                }

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

                // Voice-transcription budget scaling: a long voice memo legitimately
                // runs past the default 55s dispatch cap. `effective_dispatch` is the
                // per-task ceiling (scaled by clip duration for `voice.transcribe`,
                // default otherwise). For transcription the single provider attempt is
                // allowed the whole budget — one long attempt beats a truncated retry.
                // Pre-dispatch config loads below keep the default cap (they are quick
                // and independent of clip length).
                let effective_dispatch = effective_dispatch_timeout(&controller_task);
                let is_transcribe = controller_task.kind == TaskKind::AudioTranscribe;

                // Pre-dispatch config load: ~a dozen sequential IPC round trips
                // (GetConfig / secret fetch), none individually timeout-bound.
                // Wrap the whole load in the dispatch cap so a single stuck
                // hotel/vault round trip can't silently burn the WaitingModel
                // window before a provider is ever dispatched (RC-1).
                let mut provider_configs = match tokio::time::timeout(
                    model_dispatch_timeout(),
                    ProviderConfigs::load(&mut ipc_client),
                )
                .await
                {
                    Ok(Ok(configs)) => configs,
                    Ok(Err(err)) => {
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
                    Err(_) => {
                        emit_failure(
                            &mut ipc_client,
                            &reply,
                            Some(controller_task.kind.as_str()),
                            None,
                            config.guest_id,
                            format!(
                                "provider_timeout: config load exceeded {}s (pre-dispatch stall, no provider resolved yet)",
                                model_dispatch_timeout().as_secs()
                            ),
                        )
                        .await?;
                        continue;
                    }
                };
                if gemini_pool_enabled {
                    match tokio::time::timeout(
                        model_dispatch_timeout(),
                        refresh_gemini_credential_pool(
                            &mut ipc_client,
                            &mut gemini_pool,
                            &mut provider_configs,
                        ),
                    )
                    .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(err)) => {
                            warn!("gemini credential pool refresh failed: {err}");
                        }
                        Err(_) => {
                            warn!(
                                "gemini credential pool refresh exceeded {}s (pre-dispatch stall); continuing with existing pool state",
                                model_dispatch_timeout().as_secs()
                            );
                        }
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

                // Auxiliary-task model pinning (Model Failover Layers Slice 4).
                // Loaded fresh per task alongside `provider_configs` above so a
                // config change takes effect on the next dispatch without a
                // restart. Any load failure (IPC error or timeout) degrades to
                // `AuxModelConfig::default()` — Auto for every kind — never
                // blocks or fails the task on account of this optional feature.
                let aux_model_config = match tokio::time::timeout(
                    model_dispatch_timeout(),
                    crate::aux_model::AuxModelConfig::load(&mut ipc_client),
                )
                .await
                {
                    Ok(Ok(cfg)) => cfg,
                    Ok(Err(err)) => {
                        warn!(
                            "aux_model config load failed; treating all aux tasks as Auto: {err}"
                        );
                        crate::aux_model::AuxModelConfig::default()
                    }
                    Err(_) => {
                        warn!(
                            "aux_model config load exceeded {}s; treating all aux tasks as Auto",
                            model_dispatch_timeout().as_secs()
                        );
                        crate::aux_model::AuxModelConfig::default()
                    }
                };
                let aux_pin = crate::aux_model::AuxTaskKind::from_task_kind(controller_task.kind)
                    .and_then(|kind| aux_model_config.for_kind(kind));

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
                    // Auxiliary-task model pinning (Model Failover Layers Slice 4):
                    // if the operator configured a non-Auto `aux_model.<kind>` for
                    // this task's aux kind, resolve the pin first, then walk
                    // `fallback_chain` in order (one attempt each) on failure,
                    // handling success/failure entirely in this block via
                    // `continue`. When no pin is configured (`aux_pin` is `None`,
                    // the default), this whole `if` is skipped and control falls
                    // straight to the unmodified code below, so Auto behavior
                    // stays bit-identical to today.
                    if let Some(aux_model) = aux_pin {
                        match crate::aux_model::dispatch_aux_chain(
                            &controller_task,
                            aux_model,
                            &providers,
                        )
                        .await
                        {
                            Ok((provider_id, output)) => {
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
                                if let Some(ref gd) = graph_domain
                                    && let Err(e) = gd.observe_model_outcome(
                                        &provider_id,
                                        &local_node_id(),
                                        latency_ms,
                                        true,
                                    )
                                {
                                    warn!("observe_model_outcome (success): {e}");
                                }
                                fire_transcription_capture_fanout(
                                    &controller_task,
                                    &output,
                                    &reply,
                                    config.guest_id,
                                );
                                match output {
                                    ProviderOutput::ToolCall {
                                        tool_name,
                                        arguments,
                                    } => {
                                        emit_tool_call_response(
                                            &mut ipc_client,
                                            &reply,
                                            tool_name,
                                            arguments,
                                            None,
                                        )
                                        .await?;
                                    }
                                    output => {
                                        let response = ControllerResponseEnvelope::from_output(
                                            &controller_task,
                                            &provider_id,
                                            output,
                                        )?;
                                        emit_text_response(&mut ipc_client, &reply, response)
                                            .await?;
                                    }
                                }
                                continue;
                            }
                            Err((last_provider, err)) => {
                                if controller_task.kind == TaskKind::Embed {
                                    // Embedding is the one aux kind that degrades to
                                    // today's Auto path (the ONNX sidecar) on chain
                                    // exhaustion instead of surfacing the error — an
                                    // embedding caller (e.g. memory indexing) must
                                    // never hard-fail just because an optional pin
                                    // or fallback_chain was misconfigured. Falling
                                    // through to the unmodified code below
                                    // re-resolves via
                                    // `providers.resolve(&controller_task)` on the
                                    // ORIGINAL (unmodified provider/model) task —
                                    // the exact same resolution today's Auto path
                                    // performs.
                                    warn!(
                                        "aux fallback_chain exhausted for embedding task \
                                         (last provider tried: {:?}): {}. Degrading to \
                                         Auto (sidecar) resolution.",
                                        last_provider, err
                                    );
                                } else {
                                    error!(
                                        "aux dispatch failed for {} task (last provider tried: {:?}): {}",
                                        controller_task.kind.as_str(),
                                        last_provider,
                                        err
                                    );
                                    emit_failure(
                                        &mut ipc_client,
                                        &reply,
                                        Some(controller_task.kind.as_str()),
                                        last_provider.as_deref(),
                                        config.guest_id,
                                        format!("Provider invocation failed: {}", err),
                                    )
                                    .await?;
                                    continue;
                                }
                            }
                        }
                    }

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
                    //
                    // The per-attempt `attempt_secs` timeout below bounds a single HTTP
                    // round trip, but credential-pool key rotation (Layer 1) can chain
                    // several attempt cycles back to back on auth/rate-limit failures.
                    // `model_dispatch_timeout()` is an outer hard ceiling on the WHOLE
                    // attempt+rotation sequence so a single degraded provider (even one
                    // that keeps "making progress" by rotating keys) cannot alone consume
                    // the full WaitingModel window before the ladder engages (RC-1).
                    let provider_result = match tokio::time::timeout(effective_dispatch, async {
                        let retry = provider.retry_policy();
                        // Transcription gets the full task budget for its single
                        // attempt; all other kinds keep the provider's own per-attempt
                        // policy (leaving room for retries within the dispatch cap).
                        let attempt_secs = if is_transcribe {
                            effective_dispatch.as_secs()
                        } else {
                            provider.attempt_policy().total_secs
                        };
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
                                        let send_result = stream_ipc
                                            .send_request_with_timeout(
                                                IpcRequest::EmitTask {
                                                    target_node: reply_clone
                                                        .return_route
                                                        .node
                                                        .clone(),
                                                    target_role: reply_clone
                                                        .return_route
                                                        .role
                                                        .clone(),
                                                    target_guest_id: reply_clone
                                                        .return_route
                                                        .guest_id
                                                        .clone(),
                                                    task_json,
                                                },
                                                Duration::from_secs(10),
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
                                    if provider.id() == "gemini"
                                        && let Some(idx) = active_pool_member {
                                            gemini_pool.note_success(idx);
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
                    })
                    .await
                    {
                        Ok(inner) => inner,
                        Err(_) => Err(anyhow::anyhow!(
                            "provider_timeout: overall dispatch exceeded {}s across attempt/rotation cycles",
                            effective_dispatch.as_secs()
                        )),
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
                            if let Some(ref gd) = graph_domain
                                && let Err(e) = gd.observe_model_outcome(
                                    &provider_id,
                                    &local_node_id(),
                                    latency_ms,
                                    true,
                                )
                            {
                                warn!("observe_model_outcome (success): {e}");
                            }

                            // ── Transcription flywheel fan-out ────────────────
                            // After a successful AudioTranscribe, fire a capture
                            // envelope to role=router-listener (if enabled).
                            fire_transcription_capture_fanout(
                                &controller_task,
                                &output,
                                &reply,
                                config.guest_id,
                            );

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
                            if let Some(ref gd) = graph_domain
                                && let Err(e) = gd.observe_model_outcome(
                                    &provider_id,
                                    &local_node_id(),
                                    latency_ms,
                                    false,
                                )
                            {
                                warn!("observe_model_outcome (failure): {e}");
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
                    // Clean shutdown: finalize and close any live streaming
                    // transcription sessions before the process exits.
                    stt_sessions.shutdown().await;
                    return Ok(());
                }
                warn!("IPC recv error: {}", err);
            }
            Err(_) => {}
        }
    }
}

/// Dispatch one `voice.transcribe.stream` frame (open/chunk/end) into the
/// streaming-transcription session manager. Every OPEN failure path delivers
/// a terminal `is_final: true` error reply so the consumer never hangs;
/// chunk/end frames for unknown sessions carry no reply address and are
/// logged and dropped.
async fn handle_stream_frame(
    ipc_client: &mut PhiloticClient,
    sessions: &mut SttSessionManager,
    task_value: &Value,
    controller_guest_id: &'static str,
) {
    match parse_stream_frame(task_value) {
        Ok(StreamFrame::Open(open)) => {
            let session_id = open.stream_session_id.clone();
            // Dedicated reply IPC connection: session replies are emitted from
            // spawned session tasks and cannot share the controller's client.
            let mut sink = match IpcStreamReplySink::connect(
                controller_guest_id,
                open.reply.clone(),
            )
            .await
            {
                Ok(sink) => sink,
                Err(err) => {
                    warn!(
                        session = %session_id,
                        "transcribe-stream open: reply IPC connect failed, dropping open: {err:#}"
                    );
                    return;
                }
            };

            // Resolve the ElevenLabs API key through the same config plumbing
            // batch STT uses, bounded by the dispatch cap (RC-1 discipline).
            let api_key = match tokio::time::timeout(
                model_dispatch_timeout(),
                ProviderConfigs::load(ipc_client),
            )
            .await
            {
                Ok(Ok(configs)) => configs
                    .elevenlabs_api_key
                    .filter(|key| !key.trim().is_empty()),
                Ok(Err(err)) => {
                    let _ = sink
                        .send(
                            &session_id,
                            "",
                            true,
                            Some(&format!("provider config load failed: {err}")),
                        )
                        .await;
                    return;
                }
                Err(_) => {
                    let _ = sink
                        .send(
                            &session_id,
                            "",
                            true,
                            Some(&format!(
                                "provider_timeout: config load exceeded {}s before stream open",
                                model_dispatch_timeout().as_secs()
                            )),
                        )
                        .await;
                    return;
                }
            };
            let Some(api_key) = api_key else {
                let _ = sink
                    .send(
                        &session_id,
                        "",
                        true,
                        Some("ElevenLabs API key missing from config"),
                    )
                    .await;
                return;
            };

            let connector = Arc::new(ElevenLabsRealtimeConnector::new(api_key));
            sessions.open(open, connector, Box::new(sink)).await;
        }
        Ok(StreamFrame::Chunk {
            stream_session_id,
            audio_base64,
        }) => {
            if !sessions.chunk(&stream_session_id, audio_base64).await {
                warn!(
                    session = %stream_session_id,
                    "transcribe-stream chunk for unknown session dropped"
                );
            }
        }
        Ok(StreamFrame::End { stream_session_id }) => {
            if !sessions.end(&stream_session_id).await {
                warn!(
                    session = %stream_session_id,
                    "transcribe-stream end for unknown session ignored"
                );
            }
        }
        Err(err) => {
            warn!("malformed transcribe-stream frame: {err:#}");
            // Best effort: OPEN-shaped frames carry a reply address — use it
            // to deliver a terminal error instead of leaving the consumer to
            // hang on a session that will never open.
            if let (Some(session_id), Some(reply)) = (
                transcribe_stream::stream_session_id(task_value),
                transcribe_stream::reply_address(task_value),
            ) && let Ok(mut sink) = IpcStreamReplySink::connect(controller_guest_id, reply).await
            {
                let _ = sink
                    .send(
                        session_id,
                        "",
                        true,
                        Some(&format!("invalid stream frame: {err}")),
                    )
                    .await;
            }
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

/// After a successful `AudioTranscribe`, fire a capture envelope to
/// role=router-listener (if `PHILOTIC_ROUTER_CAPTURE_ENABLED` is set). No-op
/// for any other task kind. Shared by the default dispatch path and the aux
/// pinned/fallback-chain dispatch path (Model Failover Layers Slice 4) so
/// both preserve the same flywheel behavior on success.
fn fire_transcription_capture_fanout(
    controller_task: &ControllerTask,
    output: &ProviderOutput,
    reply: &ReplyRoute,
    guest_id: &'static str,
) {
    if controller_task.kind != TaskKind::AudioTranscribe {
        return;
    }
    let ProviderOutput::Text {
        content, model_gen, ..
    } = output
    else {
        return;
    };
    if std::env::var("PHILOTIC_ROUTER_CAPTURE_ENABLED").as_deref() != Ok("true") {
        return;
    }

    let blob_url = controller_task
        .media_attachments()
        .first()
        .and_then(|a| a.url.clone());

    let capture_json = serde_json::to_string(&json!({
        "kind": "transcription_capture",
        "session_id": reply.session_id,
        "turn_id": reply.turn_id,
        "agent_id": guest_id,
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
        role: guest_id.to_string(),
        supported_tools: Vec::new(),
    };
    tokio::spawn(async move {
        let connect = tokio::time::timeout(
            Duration::from_secs(5),
            PhiloticClient::connect(fanout_identity),
        )
        .await;
        if let Ok(Ok(mut fanout_ipc)) = connect {
            let _ = fanout_ipc
                .send_request_with_timeout(
                    IpcRequest::EmitTask {
                        target_node: local_node_id(),
                        target_role: "router-listener".to_string(),
                        target_guest_id: None,
                        task_json: capture_json,
                    },
                    Duration::from_secs(10),
                )
                .await;
        }
    });
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

    ipc_client
        .send_request_with_timeout(reply_req, Duration::from_secs(30))
        .await
        .context("emit_text_response: ipc ack failed or timed out after 30s")?;
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

    ipc_client
        .send_request_with_timeout(reply_req, Duration::from_secs(30))
        .await
        .context("emit_tool_call_response: ipc ack failed or timed out after 30s")?;
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

/// Isolation guarantee (Model Failover Layers Slice 4 — auxiliary-task model
/// pinning): auxiliary (non-cognitive) task failures must never be classified
/// in a way that engages philote's cognitive fallback ladder
/// (`classify_provider_error` / `advance_turn_to_next_fallback_tier`) or
/// persists a session `FallbackOverride`.
///
/// `classify_provider_failure` stamps `error_class` / `sub_kind` / `status`
/// purely from the error message text, without regard to capability — that
/// annotation must stay intact for model-router's OWN internal
/// retry/rotation decisions (computed separately, earlier, in the attempt
/// loop), but the WIRE payload sent to philote for the three real aux
/// capabilities (`media.analyze`, `voice.transcribe`, `text.embed`) must
/// present as an un-annotated, non-"text.generate" failure so philote's
/// `classify_provider_error` falls through to its existing capability-gated
/// default (`Unclassified` for any capability other than
/// `text.generate`/`response.generate`) instead of matching one of the
/// annotated escalation classes ahead of that check. This makes aux tasks
/// resolve and degrade independently of the cognitive ladder, as required —
/// and applies regardless of whether the failing aux task was Auto or
/// pinned, since the guarantee must hold for aux dispatch in general.
fn isolate_aux_failure_from_cognitive_ladder(
    mut payload: TaskErrorPayload,
    capability: Option<&str>,
) -> TaskErrorPayload {
    let is_aux_capability = matches!(
        capability,
        Some(k) if k == TaskKind::MediaAnalyze.as_str()
            || k == TaskKind::AudioTranscribe.as_str()
            || k == TaskKind::Embed.as_str()
    );
    if is_aux_capability {
        payload.error_class = None;
        payload.sub_kind = None;
        payload.status = None;
    }
    payload
}

async fn emit_failure(
    ipc_client: &mut PhiloticClient,
    reply: &ReplyRoute,
    capability: Option<&str>,
    provider: Option<&str>,
    guest_id: &str,
    message: String,
) -> Result<()> {
    let error_payload = isolate_aux_failure_from_cognitive_ladder(
        classify_provider_failure(capability, provider, &message),
        capability,
    );
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
    let _ = ipc_client
        .send_request_with_timeout(
            IpcRequest::PushHealEntry {
                guest_id: guest_id.to_string(),
                raw_text,
            },
            Duration::from_secs(10),
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

    ipc_client
        .send_request_with_timeout(reply_req, Duration::from_secs(30))
        .await
        .context("emit_failure: ipc ack failed or timed out after 30s")?;
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
    let _ = ipc_client
        .send_request_with_timeout(
            IpcRequest::EmitTask {
                target_node: reply.return_route.node.clone(),
                target_role: reply.return_route.role.clone(),
                target_guest_id: reply.return_route.guest_id.clone(),
                task_json,
            },
            Duration::from_secs(10),
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
    let _ = ipc_client
        .send_request_with_timeout(
            IpcRequest::EmitTask {
                target_node: reply.return_route.node.clone(),
                target_role: reply.return_route.role.clone(),
                target_guest_id: reply.return_route.guest_id.clone(),
                task_json,
            },
            Duration::from_secs(10),
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
            && (400..=599).contains(&n)
        {
            return Some(n);
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

    // Content-policy / safety block (currently Gemini-specific — see
    // `GeminiProvider::detect_content_policy_block`). Checked before every
    // other rule: this is a 2xx-with-empty-candidates outcome, not an HTTP
    // failure, so it must never fall into the generic "unclassified
    // provider_failure" bucket that philote's `classify_provider_error`
    // defaults to `SwitchProvider` (2026-07-08 forensic) — that default exists
    // for genuinely unrecognized failures, but a content block is recognized
    // right here and deserves its own non-escalating outcome: switching to a
    // different-behaving model mid-conversation is the jarring behavior this
    // classification exists to prevent.
    if message.contains("gemini_content_policy_block") {
        payload.code = Some("MODEL_CONTENT_BLOCKED".into());
        payload.sub_kind = Some("content_policy_block".into());
        // Not retryable in the classic sense (retrying the identical prompt
        // against the identical provider reproduces the identical block), but
        // this is intentionally NOT `switch_provider` — see `error_class` below.
        payload.retryable = Some(false);
        payload.error_class = Some("content_blocked".into());
        return payload;
    }

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

    // Overall dispatch / time-to-first-token cap breached — the provider made
    // no usable progress across the WHOLE budget (pre-dispatch config/credential
    // IPC load, or the full attempt+retry sequence never producing output).
    // Unlike a mid-stream `streaming_timeout` (which had already started moving
    // bytes and gets one same-tier retry), a dispatch-cap breach means this
    // provider tier is stuck from the start — go straight to the next fallback
    // tier instead of burning another full attempt cycle on the same provider.
    // (2026-07-09 stuck-turn forensic RC-1: a single slow/stuck provider must
    // not be allowed to consume the entire philote WaitingModel window before
    // the ladder engages.)
    if message.contains("provider_timeout") {
        payload.sub_kind = Some("provider_timeout".into());
        payload.retryable = Some(true);
        payload.error_class = Some("switch_provider".into());
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
// All nine parameters are flat fields of one RouterTrainingRecord, so a
// RoutingTrace struct would genuinely read better than this positional list.
// Not done here: there are nine call sites and the win is cosmetic, so it is
// left as a deliberate follow-up rather than a rushed positional rewrite.
#[allow(clippy::too_many_arguments)]
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
        agent_id: reply.agent_id.clone(),
        session_id: reply.session_id.clone(),
        turn_id: reply.turn_id.clone(),
        provider_id: provider_id.to_string(),
        model_id,
        task_kind: task_kind.to_string(),
        outcome: outcome.to_string(),
        failure_code: failure_code.map(str::to_string),
        latency_ms: Some(latency_ms),
        token_count,
        oracle_pick: reply.oracle_pick.clone(),
        oracle_agreement: reply.oracle_agreement,
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
            agent_id: task
                .get("agent_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            oracle_pick: task
                .get("oracle_pick")
                .and_then(Value::as_str)
                .map(str::to_string),
            oracle_agreement: task.get("oracle_agreement").and_then(Value::as_bool),
        }
    }
}

#[cfg(test)]
mod failure_tests {
    use super::{
        classify_provider_failure, extract_http_status, isolate_aux_failure_from_cognitive_ladder,
        model_dispatch_timeout,
    };
    use crate::controller::TaskKind;
    use std::sync::Mutex;
    use std::time::Duration;

    // ── Aux-task isolation guarantee (Slice 4) ────────────────────────────

    /// The exact aux-failure shape this slice's isolation guarantee protects
    /// against: a real, richly-annotated `classify_provider_failure` outcome
    /// (the same shape a text.generate 429 would carry, which DOES trigger
    /// philote's ladder) must be stripped down to an un-annotated payload for
    /// each of the three real aux capabilities, so philote's
    /// `classify_provider_error` falls through to its capability-gated
    /// default (`Unclassified`) instead of matching `error_class`/`sub_kind`/
    /// `status` ahead of that check.
    #[test]
    fn aux_capabilities_strip_ladder_annotations_from_rate_limited_failure() {
        for capability in [
            TaskKind::MediaAnalyze.as_str(),
            TaskKind::AudioTranscribe.as_str(),
            TaskKind::Embed.as_str(),
        ] {
            let annotated = classify_provider_failure(
                Some(capability),
                Some("gemini"),
                "Gemini API error (429): rate limit exceeded",
            );
            // Sanity: classify_provider_failure itself is NOT capability-aware —
            // it stamps the same escalation-worthy fields regardless of
            // capability. This is the raw shape the isolation guarantee acts on.
            assert_eq!(annotated.error_class.as_deref(), Some("switch_provider"));
            assert_eq!(annotated.sub_kind.as_deref(), Some("rate_limit"));

            let isolated = isolate_aux_failure_from_cognitive_ladder(annotated, Some(capability));
            assert_eq!(
                isolated.error_class, None,
                "capability [{capability}] must not carry error_class over the wire"
            );
            assert_eq!(
                isolated.sub_kind, None,
                "capability [{capability}] must not carry sub_kind over the wire"
            );
            assert_eq!(
                isolated.status, None,
                "capability [{capability}] must not carry status over the wire"
            );
            // `code`/`retryable`/`message`/`provider` are informational and
            // must NOT be stripped — only the three fields philote's
            // classify_provider_error consults for ladder escalation.
            assert_eq!(isolated.provider.as_deref(), Some("gemini"));
        }
    }

    /// Cognitive capabilities (and the `None` legacy-envelope case) must be
    /// completely unaffected — this fix is deliberately narrow-scoped to the
    /// three real aux kinds.
    #[test]
    fn cognitive_capabilities_are_not_isolated() {
        for capability in [None, Some("text.generate"), Some("response.generate")] {
            let annotated = classify_provider_failure(
                capability,
                Some("gemini"),
                "Gemini API error (429): rate limit exceeded",
            );
            let isolated = isolate_aux_failure_from_cognitive_ladder(annotated.clone(), capability);
            assert_eq!(isolated.error_class, annotated.error_class);
            assert_eq!(isolated.sub_kind, annotated.sub_kind);
            assert_eq!(isolated.status, annotated.status);
        }
    }

    /// Closes the Component A / shadow loop end to end on the model-router
    /// side: a task carrying a real `agent_id` (plus optional shadow fields)
    /// is parsed by `ReplyRoute::from_task` and then persisted by
    /// `record_routing_trace` — so the written trace carries the real agent
    /// (not the old `String::new()`) and the shadow annotations round-trip.
    #[test]
    fn from_task_threads_agent_id_and_shadow_into_recorded_trace() {
        use super::{ReplyRoute, record_routing_trace};
        use ansible_mesh_core::router_trace::{
            ProviderStats, RouterTraceStorage, RouterTrainingRecord,
        };

        #[derive(Default)]
        struct MemTrace(Mutex<Vec<RouterTrainingRecord>>);
        impl RouterTraceStorage for MemTrace {
            fn record_trace(&self, r: &RouterTrainingRecord) -> anyhow::Result<()> {
                self.0.lock().unwrap().push(r.clone());
                Ok(())
            }
            fn list_traces(&self, _: usize) -> anyhow::Result<Vec<RouterTrainingRecord>> {
                unreachable!()
            }
            fn list_traces_by_agent(
                &self,
                _: &str,
                _: usize,
            ) -> anyhow::Result<Vec<RouterTrainingRecord>> {
                unreachable!()
            }
            fn list_traces_by_provider(
                &self,
                _: &str,
                _: usize,
            ) -> anyhow::Result<Vec<RouterTrainingRecord>> {
                unreachable!()
            }
            fn provider_stats(&self, _: Option<u64>) -> anyhow::Result<Vec<ProviderStats>> {
                unreachable!()
            }
        }

        let task = serde_json::json!({
            "action": "generate_text",
            "session_id": "telegram:123:agent-jane",
            "turn_id": "turn-1",
            "chat_id": "123",
            "agent_id": "jane",
            "oracle_pick": "model.openrouter:openrouter",
            "oracle_agreement": false,
        });

        // Parse: real agent + shadow annotations land on the ReplyRoute.
        let reply = ReplyRoute::from_task(&task);
        assert_eq!(reply.agent_id, "jane");
        assert_eq!(
            reply.oracle_pick.as_deref(),
            Some("model.openrouter:openrouter")
        );
        assert_eq!(reply.oracle_agreement, Some(false));

        // Write: the recorded trace carries the real agent (Component A fix)
        // and the shadow fields — never an empty agent_id.
        let store = MemTrace::default();
        record_routing_trace(
            Some(&store),
            &reply,
            "openrouter",
            "text.generate",
            "success",
            None,
            42,
            Some("glm-5.2".into()),
            None,
        );
        let rows = store.0.lock().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].agent_id, "jane");
        assert_eq!(
            rows[0].oracle_pick.as_deref(),
            Some("model.openrouter:openrouter")
        );
        assert_eq!(rows[0].oracle_agreement, Some(false));

        // And the legacy/off path: a task with no agent_id / shadow fields
        // records an empty agent + NULL shadow (interop with old philote).
        let legacy = serde_json::json!({ "action": "generate_text", "turn_id": "t" });
        let legacy_reply = ReplyRoute::from_task(&legacy);
        assert_eq!(legacy_reply.agent_id, "");
        assert_eq!(legacy_reply.oracle_pick, None);
        assert_eq!(legacy_reply.oracle_agreement, None);
    }

    /// Serializes tests that mutate `PHILOTIC_MODEL_DISPATCH_TIMEOUT_SECS` —
    /// cargo runs tests in this module on separate threads within one
    /// process, and env vars are process-global.
    static DISPATCH_TIMEOUT_ENV_GUARD: Mutex<()> = Mutex::new(());
    fn dispatch_timeout_env_guard() -> std::sync::MutexGuard<'static, ()> {
        DISPATCH_TIMEOUT_ENV_GUARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// RC-1 (2026-07-09 stuck-turn forensic): an overall dispatch-cap breach
    /// (pre-dispatch config load, or the whole attempt+rotation sequence)
    /// must classify as `switch_provider` immediately — NOT the softer
    /// `retry_same_provider` bucket `streaming_timeout` gets, and NOT a
    /// silent unclassified MODEL_EMPTY_RESPONSE-shaped envelope. A provider
    /// that made zero progress across its entire budget should not get
    /// another same-tier cycle; the ladder should engage right away.
    #[test]
    fn classify_provider_failure_marks_provider_timeout_switch_provider() {
        let payload = classify_provider_failure(
            Some("text.generate"),
            Some("gemini"),
            "provider_timeout: overall dispatch exceeded 55s across attempt/rotation cycles",
        );

        assert_eq!(payload.kind, "provider_failure");
        assert_eq!(payload.sub_kind.as_deref(), Some("provider_timeout"));
        assert_eq!(payload.retryable, Some(true));
        assert_eq!(payload.error_class.as_deref(), Some("switch_provider"));
        // Not the mid-stream idle bucket, and not a MODEL_EMPTY_RESPONSE-style
        // unclassified code.
        assert_ne!(payload.sub_kind.as_deref(), Some("streaming_timeout"));
        assert_eq!(payload.code, None);
    }

    #[test]
    fn classify_provider_failure_config_load_timeout_message_matches_provider_timeout() {
        // Exact message shape emitted when ProviderConfigs::load times out —
        // no provider has been resolved yet at that point in dispatch.
        let payload = classify_provider_failure(
            Some("text.generate"),
            None,
            "provider_timeout: config load exceeded 55s (pre-dispatch stall, no provider resolved yet)",
        );

        assert_eq!(payload.sub_kind.as_deref(), Some("provider_timeout"));
        assert_eq!(payload.error_class.as_deref(), Some("switch_provider"));
    }

    /// A genuine mid-stream idle stall (bytes had already started moving, or
    /// the provider's own inner SSE cap fired) keeps its existing gentler
    /// same-tier-once-then-escalate treatment — this fix must not regress it.
    #[test]
    fn classify_provider_failure_streaming_timeout_still_retry_same_provider() {
        let payload = classify_provider_failure(
            Some("text.generate"),
            Some("gemini"),
            "streaming_timeout: Gemini SSE stream produced no data for 8s",
        );

        assert_eq!(payload.sub_kind.as_deref(), Some("streaming_timeout"));
        assert_eq!(payload.error_class.as_deref(), Some("retry_same_provider"));
    }

    #[test]
    fn model_dispatch_timeout_defaults_and_honors_env_override() {
        let _guard = dispatch_timeout_env_guard();
        unsafe {
            std::env::remove_var("PHILOTIC_MODEL_DISPATCH_TIMEOUT_SECS");
        }
        assert_eq!(model_dispatch_timeout(), Duration::from_secs(55));

        unsafe {
            std::env::set_var("PHILOTIC_MODEL_DISPATCH_TIMEOUT_SECS", "20");
        }
        assert_eq!(model_dispatch_timeout(), Duration::from_secs(20));

        // Invalid / zero values fall back to the default rather than
        // producing a zero-duration timeout that would fail every dispatch.
        unsafe {
            std::env::set_var("PHILOTIC_MODEL_DISPATCH_TIMEOUT_SECS", "0");
        }
        assert_eq!(model_dispatch_timeout(), Duration::from_secs(55));
        unsafe {
            std::env::set_var("PHILOTIC_MODEL_DISPATCH_TIMEOUT_SECS", "not-a-number");
        }
        assert_eq!(model_dispatch_timeout(), Duration::from_secs(55));

        unsafe {
            std::env::remove_var("PHILOTIC_MODEL_DISPATCH_TIMEOUT_SECS");
        }
    }

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

    /// The core of the second fix: a Gemini safety block (carried as the
    /// `gemini_content_policy_block` marker bailed by
    /// `GeminiProvider::detect_content_policy_block`) must classify as
    /// `content_blocked`, NOT `switch_provider` — switching to a
    /// different-behaving model mid-conversation is the jarring failover this
    /// class exists to prevent (2026-07-09 operator report).
    #[test]
    fn classify_provider_failure_marks_content_policy_block_as_content_blocked_not_switch() {
        let payload = classify_provider_failure(
            Some("text.generate"),
            Some("gemini"),
            "gemini_content_policy_block: finishReason=SAFETY",
        );

        assert_eq!(payload.sub_kind.as_deref(), Some("content_policy_block"));
        assert_eq!(payload.error_class.as_deref(), Some("content_blocked"));
        assert_ne!(payload.error_class.as_deref(), Some("switch_provider"));
        assert_ne!(payload.error_class.as_deref(), Some("retry_same_provider"));
        assert_eq!(payload.code.as_deref(), Some("MODEL_CONTENT_BLOCKED"));
    }

    #[test]
    fn classify_provider_failure_marks_prompt_feedback_block_reason_as_content_blocked() {
        let payload = classify_provider_failure(
            Some("text.generate"),
            Some("gemini"),
            "gemini_content_policy_block: promptFeedback.blockReason=SAFETY",
        );

        assert_eq!(payload.error_class.as_deref(), Some("content_blocked"));
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

    #[test]
    fn transcribe_budget_scales_with_clip_duration() {
        // Default per-task cap when no override env is set.
        let default = super::MODEL_DISPATCH_TIMEOUT_SECS_DEFAULT; // 55
        // 60s clip -> 30 + 90 = 120s.
        assert_eq!(super::transcribe_budget_secs(default, Some(60)), 120);
        // Short 10s clip -> 45s, floored up to the default 55s.
        assert_eq!(super::transcribe_budget_secs(default, Some(10)), 55);
        // Long 180s clip -> 300s, capped at 240s (< 300s watchdog).
        assert_eq!(super::transcribe_budget_secs(default, Some(180)), 240);
        // Exactly at the cap boundary: 140s -> 30 + 210 = 240.
        assert_eq!(super::transcribe_budget_secs(default, Some(140)), 240);
        // Unknown duration assumes a long clip -> lands at the cap, never the floor.
        assert_eq!(
            super::transcribe_budget_secs(default, None),
            super::TRANSCRIBE_MAX_BUDGET_SECS
        );
    }
}

#[cfg(test)]
mod openrouter_default_tests {
    use super::DEFAULT_OPENROUTER_MODEL;

    /// The OpenRouter controller must supply its own OpenRouter-valid default
    /// model. Regression guard: it must NOT fall back to the generic
    /// `OpenAIProvider` slug `gpt-4.1-mini` (nor its `openai/`-prefixed form),
    /// which is what the bin used before this fix. This pins the single const
    /// the `model-controller-openrouter` bin references.
    #[test]
    fn openrouter_default_model_is_glm_not_openai_slug() {
        assert_eq!(DEFAULT_OPENROUTER_MODEL, "z-ai/glm-5.2");
        assert_ne!(DEFAULT_OPENROUTER_MODEL, "gpt-4.1-mini");
        assert_ne!(DEFAULT_OPENROUTER_MODEL, "openai/gpt-4.1-mini");
    }
}
