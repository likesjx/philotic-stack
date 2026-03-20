use crate::controller::{
    ControllerResponseEnvelope, ControllerTask, ModelProvider, ProviderConfigs, ProviderOutput,
    ProviderRegistry, TaskKind,
};
use anyhow::Result;
use philotic_client::{
    GuestIdentity, IpcRequest, IpcResponse, PhiloticClient, TaskErrorPayload, is_ipc_disconnect,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

fn local_node_id() -> String {
    std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string())
}

type ProviderFactory =
    dyn Fn(reqwest::Client, &ProviderConfigs) -> Vec<Arc<dyn ModelProvider>> + Send + Sync;

pub struct ControllerGuestConfig {
    pub guest_id: &'static str,
    pub role: &'static str,
    /// Transitional knob from the earlier inline-audio prototype. Canonical audio delivery is
    /// now handled through the normal model-response artifact path, so this flag is ignored.
    pub allow_inline_audio: bool,
    pub providers: Box<ProviderFactory>,
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
    tracing_subscriber::fmt::init();
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

                let provider = match providers.resolve(&controller_task) {
                    Ok(provider) => provider,
                    Err(err) => {
                        emit_failure(
                            &mut ipc_client,
                            &reply,
                            Some(controller_task.kind.as_str()),
                            None,
                            format!("No model provider available for task: {}", err),
                        )
                        .await?;
                        continue;
                    }
                };

                info!(
                    "Dispatching {} task from role [{}] to provider [{}]",
                    controller_task.kind.as_str(),
                    config.role,
                    provider.id()
                );

                match provider.invoke(&controller_task).await {
                    Ok(ProviderOutput::ToolCall { tool_name, arguments }) => {
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
                        let response = ControllerResponseEnvelope::from_output(
                            &controller_task,
                            provider.id(),
                            output,
                        )?;
                        emit_text_response(&mut ipc_client, &reply, response).await?;
                    }
                    Err(err) => {
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
                            info!("Model controller turn/iteration-aware stub mode returning response for [{}].", iter_key);
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
                info!("Model controller turn-aware stub mode returning response for [{}].", turn_id);
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
        let value: Value = serde_json::from_str(json_text)
            .unwrap_or_else(|_| json!({ "display_text": trimmed }));
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
        .and_then(|task| task.composed_prompt_text().or_else(|| task.prompt_text().map(str::to_string)))
        .unwrap_or_default();
    for needle in required.iter().filter_map(Value::as_str) {
        if !prompt.contains(needle) {
            anyhow::bail!("stub validation failed: prompt missing required substring {:?}", needle);
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
    let mut payload =
        TaskErrorPayload::provider_failure("model-router", capability, provider, message.to_string());

    let malformed_tool_call = message.contains("tool_call.arguments missing from")
        || message.contains("returned invalid tool_call")
        || message.contains("returned unsupported tool_call");

    if malformed_tool_call {
        payload.code = Some("MODEL_INVALID_TOOL_CALL".into());
        payload.retryable = Some(true);
    }

    payload
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
    use super::{StubResponse, parse_stub_response, short_circuit_response, validate_stub_prompt};
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
}
