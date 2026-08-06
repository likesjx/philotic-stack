//! Hotel-internal client for the governed HTTP integration fabric.
//!
//! Trusted hotel services use this adapter instead of constructing a direct
//! `reqwest::Client`. The service connects to the local front desk as a named
//! system guest, resolves the canonical binding and placement through `aiua`,
//! dispatches the bounded request to `egress-http-runner`, and receives the
//! sanitized response over the normal task return path.

use std::time::Duration;

use ansible_mesh_core::integration::{
    EgressPlacementDecision, HttpIntegrationRequest, HttpIntegrationResponse, IntegrationBinding,
    OidcExchangeRequest, OidcExchangeResponse, projected_http_tool_name,
};
use anyhow::{Context, Result, anyhow, bail};
use philotic_client::{
    GuestIdentity, IntegrationBindingEntry, IpcRequest, IpcResponse, PhiloticClient,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::time::{Instant, timeout};
use tracing::warn;
use uuid::Uuid;

const RUNNER_ROLE: &str = "egress-http-runner";

#[derive(Debug, Clone)]
pub struct GovernedHttpService {
    pub socket_path: String,
    pub local_node_id: String,
    pub guest_id: String,
    pub role: String,
}

impl GovernedHttpService {
    pub async fn execute(
        &self,
        desired_binding: IntegrationBinding,
        request: HttpIntegrationRequest,
        operation: &str,
    ) -> Result<HttpIntegrationResponse> {
        if request.binding_id != desired_binding.binding_id {
            bail!(
                "request binding '{}' does not match desired authority '{}'",
                request.binding_id,
                desired_binding.binding_id
            );
        }
        self.execute_typed(
            desired_binding,
            request,
            |binding_id| projected_http_tool_name(binding_id),
            operation,
        )
        .await
    }

    pub async fn execute_oidc(
        &self,
        desired_binding: IntegrationBinding,
        request: OidcExchangeRequest,
        operation: &str,
    ) -> Result<OidcExchangeResponse> {
        if request.binding_id != desired_binding.binding_id {
            bail!(
                "request binding '{}' does not match desired authority '{}'",
                request.binding_id,
                desired_binding.binding_id
            );
        }
        self.execute_typed(
            desired_binding,
            request,
            |binding_id| format!("integration.auth.{binding_id}.exchange"),
            operation,
        )
        .await
    }

    async fn execute_typed<Request, Response>(
        &self,
        desired_binding: IntegrationBinding,
        request: Request,
        tool_name: impl FnOnce(&str) -> String,
        operation: &str,
    ) -> Result<Response>
    where
        Request: Serialize,
        Response: DeserializeOwned,
    {
        let mut client = PhiloticClient::connect_at(
            &self.socket_path,
            GuestIdentity {
                guest_id: self.guest_id.clone(),
                role: self.role.clone(),
                supported_tools: Vec::new(),
            },
        )
        .await
        .context("connecting governed HTTP service to hotel IPC")?;
        expect_ok(
            client
                .send_request(IpcRequest::SubscribeInbox {
                    role: self.role.clone(),
                })
                .await
                .context("subscribing governed HTTP service inbox")?,
            "subscribe governed HTTP service inbox",
        )?;

        let entry = ensure_binding(&mut client, desired_binding).await?;
        if !entry.binding.is_granted_to(&self.guest_id) {
            bail!(
                "system guest '{}' is not granted binding '{}'",
                self.guest_id,
                entry.binding.binding_id
            );
        }
        let target_node = match (&entry.placement, entry.execution_node_id.as_deref()) {
            (EgressPlacementDecision::Deny { reason }, _) => bail!("{reason}"),
            (_, Some(node)) => node.to_string(),
            _ => bail!(
                "binding '{}' has no executable placement",
                entry.binding.binding_id
            ),
        };

        let correlation_id = Uuid::new_v4().to_string();
        let session_id = format!("system:{}", self.role);
        let tool_name = tool_name(&entry.binding.binding_id);
        let task = json!({
            "action": "execute_tool",
            "tool_name": tool_name,
            "arguments": request,
            "integration_binding": entry.binding,
            "integration_placement": entry.placement,
            "session_id": session_id,
            "turn_id": correlation_id,
            "correlation_id": correlation_id,
            "chat_id": "",
            "agent_id": self.guest_id,
            "caller_role": self.role,
            "return_route": {
                "node": self.local_node_id,
                "role": self.role,
                "guest_id": self.guest_id,
                "session_id": session_id,
                "turn_id": correlation_id,
            },
            "reply_to": self.local_node_id,
            "reply_role": self.role,
            "reply_guest_id": self.guest_id,
            "final_reply_to": self.local_node_id,
            "final_reply_role": self.role,
            "final_reply_guest_id": self.guest_id,
        });
        expect_ok(
            client
                .send_request_with_timeout(
                    IpcRequest::EmitTask {
                        target_node,
                        target_role: RUNNER_ROLE.into(),
                        target_guest_id: None,
                        task_json: task.to_string(),
                    },
                    Duration::from_secs(10),
                )
                .await
                .context("dispatching governed HTTP service request")?,
            "dispatch governed HTTP service request",
        )?;

        let deadline = Instant::now()
            + Duration::from_secs(entry_timeout_secs(&task).saturating_add(10).max(15));
        let outcome: Result<Response> = async {
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    bail!(
                        "timed out waiting for governed HTTP operation '{}'",
                        operation
                    );
                }
                let message =
                    timeout(remaining, client.recv_task())
                        .await
                        .with_context(|| {
                            format!("timed out waiting for governed HTTP operation '{operation}'")
                        })??;
                let IpcResponse::InboundTask { task_json, .. } = message else {
                    continue;
                };
                let envelope: Value = serde_json::from_str(&task_json)
                    .context("invalid governed HTTP reply envelope")?;
                if envelope.get("correlation_id").and_then(Value::as_str)
                    != Some(correlation_id.as_str())
                    && envelope.get("turn_id").and_then(Value::as_str)
                        != Some(correlation_id.as_str())
                {
                    continue;
                }
                if let Some(error) = envelope.get("error") {
                    bail!("governed HTTP operation '{operation}' failed: {error}");
                }
                return serde_json::from_value(
                    envelope
                        .get("result")
                        .cloned()
                        .ok_or_else(|| anyhow!("governed HTTP reply omitted result"))?,
                )
                .with_context(|| format!("decoding governed operation '{operation}' response"));
            }
        }
        .await;

        // Close the turn this call opened, on EVERY exit path.
        //
        // Emitting the request made the hotel record a `running` session turn.
        // Nothing else will ever close it: this service is the only party that
        // knows the outcome, and it used to just return. The turn then sat until
        // `RepairStaleSessionTurns` failed it 300s later as `ZOMBIE_TURN_REPAIR`
        // — so a *successful* catalog sync (work done in ~15s) still logged a
        // ~691s "stuck turn", and a fleet of them read as timeouts for a week.
        Self::close_turn(
            &mut client,
            &session_id,
            &correlation_id,
            &outcome,
            operation,
        )
        .await;

        outcome
    }

    /// Mark this service's session turn terminal so it never reaches the
    /// stale-turn reaper. Best-effort: a failure to close is logged, never
    /// propagated, because it must not mask the operation's own result.
    async fn close_turn<Response>(
        client: &mut PhiloticClient,
        session_id: &str,
        turn_id: &str,
        outcome: &Result<Response>,
        operation: &str,
    ) {
        let request = match outcome {
            Ok(_) => IpcRequest::CompleteTask {
                task_id: Uuid::new_v4(),
                result: json!({ "session_id": session_id, "turn_id": turn_id }),
            },
            Err(err) => IpcRequest::FailTask {
                task_id: Uuid::new_v4(),
                error_code: "GOVERNED_EGRESS_FAILED".into(),
                reason: format!("governed operation '{operation}' failed: {err:#}"),
                session_id: Some(session_id.to_string()),
                turn_id: Some(turn_id.to_string()),
            },
        };
        if let Err(err) = client
            .send_request_with_timeout(request, Duration::from_secs(10))
            .await
        {
            warn!(
                session_id,
                turn_id, "governed HTTP: failed to close session turn: {err:#}"
            );
        }
    }
}

async fn ensure_binding(
    client: &mut PhiloticClient,
    mut desired: IntegrationBinding,
) -> Result<IntegrationBindingEntry> {
    if let Some(entry) = find_binding(client, &desired.binding_id).await? {
        if entry.binding.owner_agent_id != desired.owner_agent_id {
            bail!(
                "binding '{}' is owned by '{}' instead of system owner '{}'",
                desired.binding_id,
                entry.binding.owner_agent_id,
                desired.owner_agent_id
            );
        }
        let mut comparable = desired.clone();
        comparable.updated_at = entry.binding.updated_at;
        if entry.binding == comparable {
            return Ok(entry);
        }
        desired.updated_at = desired
            .updated_at
            .max(entry.binding.updated_at.saturating_add(1));
    }
    match client
        .send_request(IpcRequest::RegisterIntegrationBinding {
            binding: desired.clone(),
        })
        .await
        .context("registering governed HTTP service binding")?
    {
        IpcResponse::IntegrationBindingRegistered { binding_id, .. }
            if binding_id == desired.binding_id => {}
        other => bail!("governed HTTP binding registration failed: {other:?}"),
    }
    find_binding(client, &desired.binding_id)
        .await?
        .ok_or_else(|| anyhow!("registered binding '{}' was not listed", desired.binding_id))
}

async fn find_binding(
    client: &mut PhiloticClient,
    binding_id: &str,
) -> Result<Option<IntegrationBindingEntry>> {
    match client
        .send_request(IpcRequest::GetIntegrationBindings {})
        .await
        .context("listing governed HTTP bindings")?
    {
        IpcResponse::IntegrationBindingsState {
            integration_bindings,
        } => Ok(integration_bindings
            .into_iter()
            .find(|entry| entry.binding.binding_id == binding_id)),
        other => bail!("unexpected integration binding response: {other:?}"),
    }
}

fn expect_ok(response: IpcResponse, operation: &str) -> Result<()> {
    match response {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        other => bail!("{operation} failed: {other:?}"),
    }
}

fn entry_timeout_secs(task: &Value) -> u64 {
    task.get("integration_binding")
        .and_then(|binding| binding.get("target"))
        .and_then(|target| target.get("timeout_secs"))
        .and_then(Value::as_u64)
        .unwrap_or(30)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_reads_http_binding_limit() {
        let task = json!({
            "integration_binding": {
                "target": {
                    "kind": "http",
                    "timeout_secs": 17
                }
            }
        });
        assert_eq!(entry_timeout_secs(&task), 17);
        assert_eq!(entry_timeout_secs(&json!({})), 30);
    }
}
