use crate::controller::{
    ControllerResponseEnvelope, ControllerTask, ModelProvider, ProviderConfigs, ProviderRegistry,
    TaskKind,
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
    std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-ansible-01".to_string())
}

type ProviderFactory =
    dyn Fn(reqwest::Client, &ProviderConfigs) -> Vec<Arc<dyn ModelProvider>> + Send + Sync;

pub struct ControllerGuestConfig {
    pub guest_id: &'static str,
    pub role: &'static str,
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

                if let Some(response_text) =
                    short_circuit_response(&task_value, stub_response.as_deref())
                {
                    emit_text_response(
                        &mut ipc_client,
                        &reply,
                        ControllerResponseEnvelope {
                            capability: TaskKind::TextGenerate.as_str().to_string(),
                            content: response_text.clone(),
                            result: json!({ "display_text": response_text }),
                            artifacts: Vec::new(),
                            trace: Default::default(),
                            provider_output: Value::Null,
                        },
                    )
                    .await?;
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

                if controller_task.kind == TaskKind::VoiceSynthesize && !config.allow_inline_audio {
                    emit_failure(
                        &mut ipc_client,
                        &reply,
                        Some(controller_task.kind.as_str()),
                        None,
                        "Voice synthesis is wired as a separate model-controller guest, but canonical audio delivery is not implemented yet. Next seam: voice machine + media delivery.".into(),
                    )
                    .await?;
                    continue;
                }

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

fn short_circuit_response(task: &Value, stub_response: Option<&str>) -> Option<String> {
    if let Some(stub_text) = stub_response {
        if task.get("prompt").and_then(Value::as_str).is_some() {
            info!("Model controller stub mode returning deterministic response.");
            return Some(stub_text.to_string());
        }
    }

    None
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

async fn emit_failure(
    ipc_client: &mut PhiloticClient,
    reply: &ReplyRoute,
    capability: Option<&str>,
    provider: Option<&str>,
    message: String,
) -> Result<()> {
    let error_payload =
        TaskErrorPayload::provider_failure("model-router", capability, provider, message.clone());
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
