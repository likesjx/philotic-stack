use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ansible_mesh_core::integration::{
    parse_projected_http_tool_name, EgressPlacementDecision, HttpIntegrationAudit,
    HttpIntegrationRequest, IntegrationBinding, IntegrationTarget,
};
use anyhow::Result;
use egress_http_runner::{execute, ExecutionContext};
use philotic_client::{
    is_ipc_disconnect, GuestIdentity, IpcRequest, IpcResponse, PhiloticClient, ReturnRoute,
    TaskErrorPayload,
};
use serde_json::{json, Value};
use tracing::{info, warn};

const ROLE: &str = "egress-http-runner";

fn local_node_id() -> String {
    std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string())
}

async fn resolve_credential(
    ipc_client: &mut PhiloticClient,
    binding: &IntegrationBinding,
) -> Result<Option<String>, String> {
    let IntegrationTarget::Http(target) = &binding.target else {
        return Ok(None);
    };
    let Some(credential) = &target.credential else {
        return Ok(None);
    };
    let response = ipc_client
        .send_request_with_timeout(
            IpcRequest::GetSecret {
                secret_ref: credential.secret_ref.clone(),
            },
            Duration::from_secs(5),
        )
        .await
        .map_err(|error| format!("credential resolution failed: {error}"))?;
    match response {
        IpcResponse::SecretData {
            value_json: Some(value),
            ..
        } => {
            let decoded: String = serde_json::from_str(&value)
                .unwrap_or_else(|_| value.trim_matches('"').to_string());
            Ok(Some(decoded))
        }
        _ => Err(format!(
            "credential ref '{}' is unavailable to role '{}'",
            credential.secret_ref, ROLE
        )),
    }
}

async fn record_audit(ipc_client: &mut PhiloticClient, audit: HttpIntegrationAudit) {
    match ipc_client
        .send_request_with_timeout(
            IpcRequest::RecordIntegrationAudit {
                audit: audit.clone(),
            },
            Duration::from_secs(5),
        )
        .await
    {
        Ok(IpcResponse::Standard { ok: true, .. }) => {}
        Ok(response) => warn!(
            binding_id = audit.binding_id,
            ?response,
            "durable integration audit append was rejected"
        ),
        Err(error) => warn!(
            binding_id = audit.binding_id,
            %error,
            "durable integration audit append failed"
        ),
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn failure_code(message: &str) -> &'static str {
    let normalized = message.to_ascii_lowercase();
    if normalized.contains("credential") || normalized.contains("secret") {
        "credential_unavailable"
    } else if normalized.contains("timed out") || normalized.contains("timeout") {
        "request_timeout"
    } else if normalized.contains("response body exceeds") {
        "response_too_large"
    } else if normalized.contains("denied")
        || normalized.contains("outside")
        || normalized.contains("forbidden")
        || normalized.contains("scope")
        || normalized.contains("allow")
    {
        "policy_denied"
    } else {
        "executor_error"
    }
}

fn failed_audit(
    task: &Value,
    binding: &IntegrationBinding,
    request: &HttpIntegrationRequest,
    placement: EgressPlacementDecision,
    started_at_ms: u64,
    message: &str,
) -> HttpIntegrationAudit {
    let target = match &binding.target {
        IntegrationTarget::Http(target) => Some(target),
        IntegrationTarget::Mcp { .. } => None,
    };
    let target_origin = target
        .and_then(|target| reqwest::Url::parse(&target.base_url).ok())
        .and_then(|url| {
            Some(format!(
                "{}://{}:{}",
                url.scheme(),
                url.host_str()?,
                url.port_or_known_default()?
            ))
        })
        .unwrap_or_else(|| "invalid://binding".into());
    let finished_at_ms = unix_ms();
    let turn_id = task
        .get("turn_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    HttpIntegrationAudit {
        binding_id: binding.binding_id.clone(),
        tool_name: task
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        agent_id: task
            .get("agent_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        caller_role: task
            .get("caller_role")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        session_id: task
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        turn_id: turn_id.to_string(),
        correlation_id: task
            .get("correlation_id")
            .and_then(Value::as_str)
            .unwrap_or(turn_id)
            .to_string(),
        traffic_class: binding.traffic_class,
        executor_node_id: local_node_id(),
        placement,
        target_origin,
        method: request.method.clone(),
        path: request.path.clone(),
        policy_revision: binding.updated_at,
        approval_required: binding.requires_approval,
        credential_ref: target
            .and_then(|target| target.credential.as_ref())
            .map(|credential| credential.secret_ref.clone()),
        credential_injected: false,
        redirect_count: 0,
        request_bytes: request
            .body
            .as_ref()
            .and_then(|body| serde_json::to_vec(body).ok())
            .map(|body| body.len() as u64)
            .unwrap_or(0),
        response_status: None,
        response_bytes: 0,
        started_at_ms,
        finished_at_ms,
        duration_ms: finished_at_ms.saturating_sub(started_at_ms),
        outcome: "failed".into(),
        failure_code: Some(failure_code(message).into()),
    }
}

async fn handle_call(ipc_client: &mut PhiloticClient, task: &Value) -> Result<()> {
    let started_at_ms = unix_ms();
    let return_route = ReturnRoute::from_task(task, local_node_id(), "agent");
    let tool_name = task
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let agent_id = task
        .get("agent_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let chat_id = task
        .get("chat_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let caller_role = task
        .get("caller_role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let session_id = task
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let turn_id = task
        .get("turn_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let correlation_id = task
        .get("correlation_id")
        .and_then(Value::as_str)
        .unwrap_or(turn_id);
    let binding: Result<IntegrationBinding, _> = serde_json::from_value(
        task.get("integration_binding")
            .cloned()
            .unwrap_or(Value::Null),
    );
    let placement: EgressPlacementDecision = serde_json::from_value(
        task.get("integration_placement")
            .cloned()
            .unwrap_or_else(|| json!({"decision": "execute_local", "audit_fallback": false})),
    )
    .unwrap_or_else(|_| EgressPlacementDecision::Deny {
        reason: "task omitted a valid placement decision".into(),
    });
    let request: Result<HttpIntegrationRequest, _> =
        serde_json::from_value(task.get("arguments").cloned().unwrap_or(json!({})));

    let outcome = match (&binding, &request) {
        (Ok(binding), Ok(request)) => {
            let mut request = request.clone();
            if request.binding_id.is_empty() {
                if let Some(parsed) = parse_projected_http_tool_name(&tool_name) {
                    request.binding_id = parsed.to_string();
                }
            }
            if !binding.is_granted_to(agent_id) {
                Err(format!(
                    "agent '{}' is not granted integration '{}'",
                    agent_id, binding.binding_id
                ))
            } else if matches!(&placement, EgressPlacementDecision::Deny { .. }) {
                Err("hotel supplied a denied integration placement".into())
            } else {
                match resolve_credential(ipc_client, &binding).await {
                    Ok(credential) => execute(
                        &binding,
                        &request,
                        ExecutionContext {
                            executor_node_id: &local_node_id(),
                            placement: placement.clone(),
                            credential: credential.as_deref(),
                            tool_name: &tool_name,
                            agent_id,
                            caller_role,
                            session_id,
                            turn_id,
                            correlation_id,
                        },
                    )
                    .await
                    .map_err(|error| format!("{error:#}")),
                    Err(message) => Err(message),
                }
            }
        }
        (Err(error), _) => Err(format!("task omitted a valid integration binding: {error}")),
        (_, Err(error)) => Err(format!("invalid HTTP integration arguments: {error}")),
    };

    let mut reply = json!({
        "action": "datasource_response",
        "capability": "integration.http.request",
        "tool_name": tool_name,
        "provider": ROLE,
        "return_route": {
            "node": return_route.node,
            "role": return_route.role,
            "guest_id": return_route.guest_id,
            "session_id": return_route.session_id,
            "turn_id": return_route.turn_id,
        },
        "reply_guest_id": return_route.guest_id,
        "session_id": return_route.session_id,
        "turn_id": return_route.turn_id,
        "chat_id": chat_id,
    });
    match outcome {
        Ok(result) => {
            record_audit(ipc_client, result.audit.clone()).await;
            reply["result"] = serde_json::to_value(result).unwrap_or(Value::Null);
        }
        Err(message) => {
            if let (Ok(binding), Ok(request)) = (&binding, &request) {
                record_audit(
                    ipc_client,
                    failed_audit(task, binding, request, placement, started_at_ms, &message),
                )
                .await;
            }
            reply["error"] = serde_json::to_value(TaskErrorPayload::provider_failure(
                ROLE,
                Some("integration.http.request"),
                Some(ROLE),
                message,
            ))
            .unwrap_or(Value::Null);
        }
    }
    ipc_client
        .send_request_with_timeout(
            IpcRequest::EmitTask {
                target_node: return_route.node,
                target_role: return_route.role,
                target_guest_id: return_route.guest_id,
                task_json: reply.to_string(),
            },
            Duration::from_secs(10),
        )
        .await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "egress_http_runner=info".into()),
        )
        .init();
    let guest_id = std::env::var("PHILOTIC_GUEST_ID").unwrap_or_else(|_| "egress-http".to_string());
    let identity = GuestIdentity {
        guest_id: guest_id.clone(),
        role: ROLE.into(),
        supported_tools: Vec::new(),
    };
    let mut ipc_client = PhiloticClient::connect(identity).await?;
    ipc_client
        .send_request(IpcRequest::SubscribeInbox { role: ROLE.into() })
        .await?;
    info!(guest_id, role = ROLE, "bounded HTTP runner listening");

    loop {
        match ipc_client.recv_task().await {
            Ok(IpcResponse::InboundTask { task_json, .. }) => {
                match serde_json::from_str::<Value>(&task_json) {
                    Ok(task)
                        if task.get("action").and_then(Value::as_str) == Some("execute_tool") =>
                    {
                        if let Err(error) = handle_call(&mut ipc_client, &task).await {
                            warn!(%error, "HTTP integration call handling failed");
                        }
                    }
                    Ok(task) => warn!(?task, "ignoring unsupported runner task"),
                    Err(error) => warn!(%error, "invalid runner task JSON"),
                }
            }
            Ok(_) => {}
            Err(error) if is_ipc_disconnect(&error) => return Err(error),
            Err(error) => warn!(%error, "runner inbox receive failed"),
        }
    }
}
