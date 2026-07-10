use crate::controller::{
    CONTRACT_ERROR_MARKER, DatasourceProvider, DatasourceTask, ProviderOutput, ProviderRegistry,
};
use anyhow::Result;
use philotic_client::{
    GuestIdentity, IpcRequest, IpcResponse, PhiloticClient, ReturnRoute, TaskErrorPayload,
    is_ipc_disconnect,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

pub type ProviderFactory = dyn Fn() -> Vec<Arc<dyn DatasourceProvider>> + Send + Sync;

pub struct DatasourceGuestConfig {
    pub guest_id: &'static str,
    pub role: &'static str,
    pub providers: Box<ProviderFactory>,
}

#[derive(Debug, Clone)]
struct ReplyRoute {
    return_route: ReturnRoute,
    chat_id: String,
}

impl ReplyRoute {
    fn from_task(task: &Value) -> Self {
        let local_node_id =
            std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string());

        let return_route = ReturnRoute::from_task(task, local_node_id, "agent");

        Self {
            return_route,
            chat_id: task
                .get("chat_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }
    }
}

pub async fn run_datasource_controller(config: DatasourceGuestConfig) -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();

    info!(
        guest_id = config.guest_id,
        role = config.role,
        "starting datasource guest controller"
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

    info!(role = config.role, "listening for datasource tasks");

    loop {
        match tokio::time::timeout(Duration::from_secs(5), ipc_client.recv_task()).await {
            Ok(Ok(IpcResponse::InboundTask {
                source_node,
                task_id,
                task_json,
            })) => {
                info!(
                    guest_id = config.guest_id,
                    task_id = %task_id,
                    source_node,
                    "received datasource task"
                );

                let task_value = match serde_json::from_str::<Value>(&task_json) {
                    Ok(task) => task,
                    Err(err) => {
                        warn!("failed to parse inbound datasource task JSON: {err}");
                        continue;
                    }
                };

                let reply = ReplyRoute::from_task(&task_value);
                let controller_task = match DatasourceTask::from_value(&task_value) {
                    Ok(task) => task,
                    Err(err) => {
                        emit_failure(
                            &mut ipc_client,
                            &reply,
                            None,
                            None,
                            format!("uninterpretable datasource task: {err}"),
                        )
                        .await?;
                        continue;
                    }
                };

                let providers = ProviderRegistry::new((config.providers)());
                let provider = match providers.resolve(&controller_task) {
                    Ok(provider) => provider,
                    Err(err) => {
                        emit_failure(
                            &mut ipc_client,
                            &reply,
                            Some(controller_task.kind.as_str()),
                            None,
                            format!("no datasource provider available: {err}"),
                        )
                        .await?;
                        continue;
                    }
                };

                info!(
                    capability = controller_task.kind.as_str(),
                    provider = provider.id(),
                    "dispatching datasource task"
                );

                match provider.invoke(&controller_task).await {
                    Ok(output) => {
                        emit_success_response(
                            &mut ipc_client,
                            &reply,
                            &controller_task,
                            provider.id(),
                            output,
                        )
                        .await?;
                    }
                    Err(err) => {
                        // Use the alternate/chain Display ({err:#}) so anyhow's
                        // .context() cause chain (e.g. the serde field/type mismatch
                        // that produced a parse failure) reaches the log and the
                        // caller instead of being swallowed by the top-level message.
                        let chained = format!("{err:#}");
                        error!("datasource provider invocation failed: {chained}");
                        // Provider handlers mark pre-write, model-fixable
                        // contract/parameter failures with CONTRACT_ERROR_MARKER
                        // (see data-memorygraphrag's handle_observe). Tag those as
                        // "invalid_request" so philote can tell a contract failure
                        // (worth one bounded model retry) apart from an infra/
                        // transport failure, without guessing from free-text.
                        let sub_kind = if chained.contains(CONTRACT_ERROR_MARKER) {
                            Some("invalid_request")
                        } else {
                            None
                        };
                        emit_failure_with_sub_kind(
                            &mut ipc_client,
                            &reply,
                            Some(controller_task.kind.as_str()),
                            Some(provider.id()),
                            format!("provider failed: {chained}"),
                            sub_kind,
                        )
                        .await?;
                    }
                }
            }
            Ok(Ok(other)) => {
                info!(
                    ?other,
                    "received non-task IPC while running datasource guest"
                );
            }
            Ok(Err(err)) => {
                if is_ipc_disconnect(&err) {
                    info!(
                        guest_id = config.guest_id,
                        "hotel IPC disconnected; datasource guest exiting"
                    );
                    return Ok(());
                }
                warn!("datasource guest IPC receive error: {err}");
            }
            Err(_) => {}
        }
    }
}

async fn emit_success_response(
    ipc_client: &mut PhiloticClient,
    reply: &ReplyRoute,
    task: &DatasourceTask,
    provider_id: &str,
    output: ProviderOutput,
) -> Result<()> {
    let result_json = match output {
        ProviderOutput::ResultSet(value) => json!({"status": "success", "data": value}),
        ProviderOutput::PartitionCreated { graph_id } => {
            json!({"status": "created", "graph_id": graph_id})
        }
        ProviderOutput::Acknowledge => json!({"status": "acknowledged"}),
    };

    ipc_client
        .send_request(IpcRequest::EmitTask {
            target_node: reply.return_route.node.clone(),
            target_role: reply.return_route.role.clone(),
            target_guest_id: reply.return_route.guest_id.clone(),
            task_json: json!({
                "action": "datasource_response",
                "capability": task.kind.as_str(),
                "tool_name": task.kind.as_str(),
                "provider": provider_id,
                "return_route": reply.return_route.as_json(),
                "reply_guest_id": reply.return_route.guest_id,
                "session_id": reply.return_route.session_id,
                "turn_id": reply.return_route.turn_id,
                "chat_id": reply.chat_id,
                "result": result_json,
            })
            .to_string(),
        })
        .await?;

    Ok(())
}

async fn emit_failure(
    ipc_client: &mut PhiloticClient,
    reply: &ReplyRoute,
    capability: Option<&str>,
    provider: Option<&str>,
    message: String,
) -> Result<()> {
    emit_failure_with_sub_kind(ipc_client, reply, capability, provider, message, None).await
}

/// Same as [`emit_failure`], but lets the caller tag the failure with a
/// `sub_kind` (e.g. `"invalid_request"` for parameter/parse contract
/// failures) so downstream consumers like philote can distinguish a
/// malformed-parameters failure — worth one bounded model self-correction
/// retry — from a transport/routing failure, without string-matching
/// `message`.
async fn emit_failure_with_sub_kind(
    ipc_client: &mut PhiloticClient,
    reply: &ReplyRoute,
    capability: Option<&str>,
    provider: Option<&str>,
    message: String,
    sub_kind: Option<&str>,
) -> Result<()> {
    let mut payload =
        TaskErrorPayload::provider_failure("datasource_controller", capability, provider, message);
    payload.sub_kind = sub_kind.map(str::to_string);

    ipc_client
        .send_request(IpcRequest::EmitTask {
            target_node: reply.return_route.node.clone(),
            target_role: reply.return_route.role.clone(),
            target_guest_id: reply.return_route.guest_id.clone(),
            task_json: json!({
                "action": "datasource_response",
                "capability": capability.unwrap_or("unknown"),
                "tool_name": capability.unwrap_or("unknown"),
                "provider": provider.unwrap_or("unknown"),
                "return_route": reply.return_route.as_json(),
                "reply_guest_id": reply.return_route.guest_id,
                "session_id": reply.return_route.session_id,
                "turn_id": reply.return_route.turn_id,
                "chat_id": reply.chat_id,
                "error": payload,
            })
            .to_string(),
        })
        .await?;

    Ok(())
}
