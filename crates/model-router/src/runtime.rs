use crate::controller::{
    ControllerResponseEnvelope, ControllerTask, ModelProvider, NativeLiveProvider,
    NativeLiveRegistry, ProviderConfigs, ProviderOutput, ProviderRegistry, TaskKind,
};
use ansible_mesh_core::router_trace::{
    RouterTraceStorage, RouterTrainingRecord, SqliteRouterTraceStorage,
};
use anyhow::Result;
use philotic_client::{
    GuestIdentity, IpcRequest, IpcResponse, PhiloticClient, TaskErrorPayload, is_ipc_disconnect,
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
    reply_to: String,
    reply_role: String,
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

    // Open the router training-tap trace store if configured.
    let trace_store: Option<Arc<dyn RouterTraceStorage>> =
        match std::env::var("PHILOTIC_ROUTER_TRACE_DB") {
            Ok(path) => {
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
            }
            Err(_) => None,
        };

    info!(
        "Listening for inbound model tasks on role [{}] from the Philotic Web...",
        config.role
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
                            format!("Model controller could not interpret task: {}", err),
                        )
                        .await?;
                        continue;
                    }
                };

                let provider_configs = match ProviderConfigs::load(&mut ipc_client).await {
                    Ok(configs) => configs,
                    Err(err) => {
                        emit_failure(
                            &mut ipc_client,
                            &reply,
                            Some(controller_task.kind.as_str()),
                            None,
                            format!(
                                "Model controller failed to refresh provider config: {}",
                                err
                            ),
                        )
                        .await?;
                        continue;
                    }
                };
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
                            );
                            error!("Native-live provider invocation failed: {}", err);
                            emit_failure(
                                &mut ipc_client,
                                &reply,
                                Some(controller_task.kind.as_str()),
                                Some(provider.id()),
                                format!("Native-live provider invocation failed: {}", err),
                            )
                            .await?;
                        }
                    }
                } else {
                    let provider = match providers.resolve(&controller_task) {
                        Ok(provider) => provider,
                        Err(err) => {
                            // This controller has no provider for this task kind. Skip silently —
                            // another controller on the same role inbox may support it.
                            info!(
                                "Controller [{}] skipping {} task: {}",
                                config.guest_id,
                                controller_task.kind.as_str(),
                                err
                            );
                            continue;
                        }
                    };

                    info!(
                        "Dispatching {} task from role [{}] to provider [{}]",
                        controller_task.kind.as_str(),
                        config.role,
                        provider.id()
                    );

                    let provider_id = provider.id().to_string();

                    // ── Streaming dispatch ────────────────────────────────────
                    // When the provider supports streaming for this task, spawn a
                    // background task that forwards tokens to philote via EmitTask.
                    // The main await still receives the final ProviderOutput.
                    let provider_result = if provider.supports_streaming(&controller_task) {
                        let (token_tx, mut token_rx) =
                            tokio::sync::mpsc::channel::<String>(128);

                        // Connect the stream IPC client BEFORE starting the SSE fetch so
                        // the forwarding task is ready to drain tokens the moment they
                        // arrive.  If we connected lazily (inside the spawned task) there
                        // is a race where invoke_streaming completes and drops token_tx
                        // before the task finishes connecting — the channel closes and no
                        // tokens are ever forwarded.
                        let stream_identity = GuestIdentity {
                            guest_id: format!("model-stream-{}", Ulid::new()),
                            role: config.guest_id.to_string(),
                            supported_tools: Vec::new(),
                        };
                        let stream_ipc_opt = PhiloticClient::connect(stream_identity).await.ok();
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
                                    "session_id": reply_clone.session_id,
                                    "turn_id": reply_clone.turn_id,
                                    "chat_id": reply_clone.chat_id,
                                    "content": token,
                                }))
                                .unwrap_or_default();
                                let _ = stream_ipc
                                    .send_request(IpcRequest::EmitTask {
                                        target_node: reply_clone.reply_to.clone(),
                                        target_role: reply_clone.reply_role.clone(),
                                        target_guest_id: None,
                                        task_json,
                                    })
                                    .await;
                            }
                        });
                        provider.invoke_streaming(&controller_task, token_tx).await
                    } else {
                        provider.invoke(&controller_task).await
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
                            record_routing_trace(
                                trace_store.as_deref(),
                                &reply,
                                &provider_id,
                                &task_kind,
                                "success",
                                None,
                                latency_ms,
                            );

                            // ── Transcription flywheel fan-out ────────────────
                            // After a successful AudioTranscribe, fire a capture
                            // envelope to role=router-listener (if enabled).
                            if controller_task.kind == TaskKind::AudioTranscribe {
                                if let ProviderOutput::Text { ref content, ref model_gen, .. } = output {
                                    if std::env::var("PHILOTIC_ROUTER_CAPTURE_ENABLED").as_deref() == Ok("true") {
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
                                            if let Ok(mut fanout_ipc) = PhiloticClient::connect(fanout_identity).await {
                                                let _ = fanout_ipc
                                                    .send_request(IpcRequest::EmitTask {
                                                        target_node: local_node_id(),
                                                        target_role: "router-listener".to_string(),
                                                        target_guest_id: None,
                                                        task_json: capture_json,
                                                    })
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
                            );
                            error!("Provider invocation failed: {}", err);
                            emit_failure(
                                &mut ipc_client,
                                &reply,
                                Some(controller_task.kind.as_str()),
                                Some(provider.id()),
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
        target_node: reply.reply_to.clone(),
        target_role: reply.reply_role.clone(),
        target_guest_id: None,
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

    ipc_client.send_request(reply_req).await?;
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
        target_node: reply.reply_to.clone(),
        target_role: reply.reply_role.clone(),
        target_guest_id: None,
        task_json: json!({
            "action": "model_response",
            "agent_action": {
                "kind": "tool_call",
                "tool_name": tool_name,
                "arguments": arguments,
                "model_result": model_result,
            },
            "session_id": reply.session_id,
            "turn_id": reply.turn_id,
            "chat_id": reply.chat_id,
            "final_reply_to": reply.final_reply_to,
            "final_reply_role": reply.final_reply_role,
            "final_reply_guest_id": reply.final_reply_guest_id
        })
        .to_string(),
    };

    ipc_client.send_request(reply_req).await?;
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
    message: String,
) -> Result<()> {
    let error_payload = classify_provider_failure(capability, provider, &message);
    error!(
        "Emitting model failure capability={:?} provider={:?}: {}",
        capability, provider, message
    );
    let reply_req = IpcRequest::EmitTask {
        target_node: reply.reply_to.clone(),
        target_role: reply.reply_role.clone(),
        target_guest_id: None,
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

    ipc_client.send_request(reply_req).await?;
    Ok(())
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

    let malformed_tool_call = message.contains("tool_call.arguments missing from")
        || message.contains("returned invalid tool_call")
        || message.contains("returned unsupported tool_call");

    if malformed_tool_call {
        payload.code = Some("MODEL_INVALID_TOOL_CALL".into());
        payload.retryable = Some(true);
        payload.sub_kind = Some("content_error".into());
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
        return payload;
    }

    // Streaming idle timeout — emitted by providers when the SSE stream stalls.
    if message.contains("streaming_timeout") {
        payload.sub_kind = Some("streaming_timeout".into());
        payload.retryable = Some(true);
        return payload;
    }

    // Rate limit (HTTP 429).
    if message.contains("429") || message.contains("rate limit") || message.contains("quota") {
        payload.sub_kind = Some("rate_limit".into());
        payload.retryable = Some(true);
        return payload;
    }

    // Generic provider-side HTTP error (5xx or non-retryable 4xx).
    if message.contains("500")
        || message.contains("502")
        || message.contains("503")
        || message.contains("504")
    {
        payload.sub_kind = Some("provider_error".into());
        payload.retryable = Some(true);
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
) {
    let Some(store) = store else { return };
    let record = RouterTrainingRecord {
        trace_id: Ulid::new().to_string(),
        agent_id: String::new(), // populated below if available from session context
        session_id: reply.session_id.clone(),
        turn_id: reply.turn_id.clone(),
        provider_id: provider_id.to_string(),
        model_id: None,
        task_kind: task_kind.to_string(),
        outcome: outcome.to_string(),
        failure_code: failure_code.map(str::to_string),
        latency_ms: Some(latency_ms),
        timestamp: now_epoch_secs(),
    };
    if let Err(e) = store.record_trace(&record) {
        warn!(provider = %provider_id, outcome = %outcome, "router trace write failed: {e}");
    }
}

impl ReplyRoute {
    fn from_task(task: &Value) -> Self {
        let local_node_id = local_node_id();
        Self {
            reply_to: task
                .get("reply_to")
                .and_then(Value::as_str)
                .unwrap_or(&local_node_id)
                .to_string(),
            reply_role: task
                .get("reply_role")
                .and_then(Value::as_str)
                .unwrap_or("agent")
                .to_string(),
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
    use super::classify_provider_failure;

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
