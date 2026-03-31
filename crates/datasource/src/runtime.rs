use crate::controller::{DatasourceProvider, DatasourceTask, ProviderOutput, ProviderRegistry};
use anyhow::Result;
use philotic_client::{
    is_ipc_disconnect, GuestIdentity, IpcRequest, IpcResponse, PhiloticClient, TaskErrorPayload,
};
use serde_json::{json, Value};
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
    tracing_subscriber::fmt::init();
    info!(
        "Starting Materialized Datasource Guest [{}] for role [{}]...",
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

    info!(
        "Listening for inbound datasource tasks on role [{}]...",
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
                    "Datasource controller [{}] received task [{}] from [{}]",
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

                let controller_task = match DatasourceTask::from_value(&task_value) {
                    Ok(task) => task,
                    Err(err) => {
                        emit_failure(
                            &mut ipc_client,
                            &reply,
                            None,
                            None,
                            format!("Uninterpretable datasource task: {}", err),
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
                            format!("No datasource provider available: {}", err),
                        )
                        .await?;
                        continue;
                    }
                };

                info!(
                    "Dispatching {} task to provider [{}]",
                    controller_task.kind.as_str(),
                    provider.id()
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
                        error!("Provider invocation failed: {}", err);
                        emit_failure(
                            &mut ipc_client,
                            &reply,
                            Some(controller_task.kind.as_str()),
                            Some(provider.id()),
                            format!("Provider failed: {}", err),
                        )
                        .await?;
                    }
                }
            }
            Ok(Ok(other)) => {
                info!("Datasource controller received non-task IPC: {:?}", other);
            }
            Ok(Err(err)) => {
                if is_ipc_disconnect(&err) {
                    info!(
                        "Hotel IPC disconnected; datasource [{}] exiting.",
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

    let reply_req = IpcRequest::EmitTask {
        target_node: reply.reply_to.clone(),
        target_role: reply.reply_role.clone(),
        target_guest_id: None,
        task_json: json!({
            "action": "datasource_response",
            "capability": task.kind.as_str(),
            "provider": provider_id,
            "session_id": reply.session_id,
            "turn_id": reply.turn_id,
            "chat_id": reply.chat_id,
            "result": result_json
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
    let payload = TaskErrorPayload::provider_failure(
        "datasource_controller",
        capability,
        provider.clone(),
        message.clone(),
    );

    let reply_req = IpcRequest::EmitTask {
        target_node: reply.reply_to.clone(),
        target_role: reply.reply_role.clone(),
        target_guest_id: None,
        task_json: json!({
            "action": "datasource_response",
            "capability": capability.unwrap_or("unknown"),
            "provider": provider.unwrap_or("unknown"),
            "session_id": reply.session_id,
            "turn_id": reply.turn_id,
            "chat_id": reply.chat_id,
            "error": payload
        })
        .to_string(),
    };

    ipc_client.send_request(reply_req).await?;
    Ok(())
}
