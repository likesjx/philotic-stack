use crate::controller::{DatasourceProvider, DatasourceTask, ProviderOutput, ProviderRegistry};
use anyhow::Result;
use philotic_client::{
    GuestIdentity, IpcRequest, IpcResponse, PhiloticClient, TaskErrorPayload, is_ipc_disconnect,
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
    reply_to: String,
    reply_role: String,
    session_id: String,
    turn_id: String,
    chat_id: String,
}

impl ReplyRoute {
    fn from_task(task: &Value) -> Self {
        let local_node_id =
            std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string());

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
                        error!("datasource provider invocation failed: {err}");
                        emit_failure(
                            &mut ipc_client,
                            &reply,
                            Some(controller_task.kind.as_str()),
                            Some(provider.id()),
                            format!("provider failed: {err}"),
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
            target_node: reply.reply_to.clone(),
            target_role: reply.reply_role.clone(),
            target_guest_id: None,
            task_json: json!({
                "action": "datasource_response",
                "capability": task.kind.as_str(),
                "tool_name": task.kind.as_str(),
                "provider": provider_id,
                "session_id": reply.session_id,
                "turn_id": reply.turn_id,
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
    let payload =
        TaskErrorPayload::provider_failure("datasource_controller", capability, provider, message);

    ipc_client
        .send_request(IpcRequest::EmitTask {
            target_node: reply.reply_to.clone(),
            target_role: reply.reply_role.clone(),
            target_guest_id: None,
            task_json: json!({
                "action": "datasource_response",
                "capability": capability.unwrap_or("unknown"),
                "tool_name": capability.unwrap_or("unknown"),
                "provider": provider.unwrap_or("unknown"),
                "session_id": reply.session_id,
                "turn_id": reply.turn_id,
                "chat_id": reply.chat_id,
                "error": payload,
            })
            .to_string(),
        })
        .await?;

    Ok(())
}
