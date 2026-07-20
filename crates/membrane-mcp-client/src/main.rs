//! membrane-mcp-client — the hotel's MCP *client* guest.
//!
//! The consumer half of the MCP fabric (proposal `mcp-client-fabric`): where
//! `membrane-mcp` serves hotel tools to external MCP clients, this guest
//! connects *outbound* to registered upstream MCP servers, projects their
//! allowlisted tools back to the hotel, and executes `mcp:<upstream>.<tool>`
//! calls dispatched by philotes.
//!
//! Runs the plain inbox-subscriber loop (datasource pattern): one guest per
//! hotel, role `mcp-client-runner`, subscribed to its own role inbox. Replies
//! ride the standard `datasource_response` shape so the philote turn resumes
//! through the existing re-entry path with no new routing code.

mod upstream;

use ansible_mesh_core::mcp_upstream::{
    McpUpstreamCatalog, McpUpstreamConfig, parse_projected_tool_name,
};
use anyhow::Result;
use philotic_client::{
    GuestIdentity, IpcRequest, IpcResponse, PhiloticClient, ReturnRoute, TaskErrorPayload,
    is_ipc_disconnect,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};
use upstream::UpstreamClient;

const ROLE: &str = "mcp-client-runner";

fn unix_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn local_node_id() -> String {
    std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string())
}

/// Per-(upstream, tool) sliding-hour allotment tracking. In-memory; resets on
/// restart (same tradeoff as the membrane-mcp inbound allotments).
#[derive(Default)]
struct Allotments {
    windows: HashMap<(String, String), Vec<u64>>,
}

impl Allotments {
    /// Charge one call; returns false if the grant's hourly budget is exhausted.
    fn charge(&mut self, upstream_id: &str, tool: &str, budget: Option<u32>) -> bool {
        let Some(budget) = budget else { return true };
        let now = unix_ts();
        let window = self
            .windows
            .entry((upstream_id.to_string(), tool.to_string()))
            .or_default();
        window.retain(|t| now.saturating_sub(*t) < 3600);
        if window.len() >= budget as usize {
            return false;
        }
        window.push(now);
        true
    }
}

struct Guest {
    ipc_client: PhiloticClient,
    upstreams: HashMap<String, UpstreamClient>,
    allotments: Allotments,
    /// Unix ts of the last connect/refresh per upstream, for
    /// `refresh_interval_secs` scheduling.
    last_refresh: HashMap<String, u64>,
}

impl Guest {
    /// Resolve the upstream's bearer credential from the hotel vault, if any.
    async fn resolve_bearer(&mut self, config: &McpUpstreamConfig) -> Option<String> {
        let secret_ref = config.credential_ref.clone()?;
        match self
            .ipc_client
            .send_request_with_timeout(
                IpcRequest::GetSecret {
                    secret_ref: secret_ref.clone(),
                },
                Duration::from_secs(5),
            )
            .await
        {
            Ok(IpcResponse::SecretData {
                value_json: Some(json),
                ..
            }) => {
                // Stored plaintext may itself be JSON-encoded (same double-
                // encoding quirk the membrane-mcp vault resolver handles).
                let step1: String = serde_json::from_str(&json)
                    .unwrap_or_else(|_| json.trim_matches('"').to_string());
                Some(step1)
            }
            Ok(_) => {
                warn!(
                    upstream = config.upstream_id,
                    secret_ref, "upstream credential not found in vault"
                );
                None
            }
            Err(e) => {
                warn!(upstream = config.upstream_id, err = %e, "credential fetch failed");
                None
            }
        }
    }

    /// (Re)connect one upstream from a fresh config and report its catalog.
    /// A config update is the approval event, so the listing becomes the new
    /// approved baseline for stale-grant detection.
    async fn sync_upstream(&mut self, config: McpUpstreamConfig) {
        let upstream_id = config.upstream_id.clone();
        let bearer = self.resolve_bearer(&config).await;
        let mut client = match UpstreamClient::new(config, bearer) {
            Ok(c) => c,
            Err(e) => {
                warn!(upstream = upstream_id, err = %e, "upstream client build failed");
                return;
            }
        };
        let outcome = client.connect_and_list(true).await;
        self.upstreams.insert(upstream_id.clone(), client);
        self.report_outcome(&upstream_id, outcome).await;
    }

    /// Periodic re-list for one connected upstream, diffing against the
    /// approved baseline (changed tools go stale, never silently re-project).
    async fn refresh_upstream(&mut self, upstream_id: &str) {
        let Some(client) = self.upstreams.get_mut(upstream_id) else {
            return;
        };
        let outcome = client.connect_and_list(false).await;
        let upstream_id = upstream_id.to_string();
        self.report_outcome(&upstream_id, outcome).await;
    }

    async fn report_outcome(&mut self, upstream_id: &str, outcome: upstream::ConnectOutcome) {
        self.last_refresh.insert(upstream_id.to_string(), unix_ts());
        let catalog = McpUpstreamCatalog {
            upstream_id: upstream_id.to_string(),
            state: outcome.state,
            tools: outcome.tools,
            missing_grants: outcome.missing_grants,
            stale_grants: outcome.stale_grants,
            reported_at: unix_ts(),
        };
        if let Err(e) = self
            .ipc_client
            .send_request_with_timeout(
                IpcRequest::ReportMcpUpstreamCatalog { catalog },
                Duration::from_secs(10),
            )
            .await
        {
            warn!(upstream = upstream_id, err = %e, "catalog report failed");
        }
    }

    /// Kick refreshes for every upstream whose interval has elapsed.
    async fn run_due_refreshes(&mut self) {
        let now = unix_ts();
        let due: Vec<String> = self
            .upstreams
            .iter()
            .filter_map(|(id, client)| {
                let interval = client.config.refresh_interval_secs?;
                let last = self.last_refresh.get(id).copied().unwrap_or(0);
                (now.saturating_sub(last) >= interval.max(30)).then(|| id.clone())
            })
            .collect();
        for id in due {
            self.refresh_upstream(&id).await;
        }
    }

    /// Startup replay: fetch every registered upstream and connect.
    async fn replay_upstreams(&mut self) {
        match self
            .ipc_client
            .send_request_with_timeout(IpcRequest::GetMcpUpstreams {}, Duration::from_secs(5))
            .await
        {
            Ok(IpcResponse::McpUpstreamsState { mcp_upstreams }) => {
                info!(count = mcp_upstreams.len(), "replaying registered upstreams");
                for entry in mcp_upstreams {
                    self.sync_upstream(entry.config).await;
                }
            }
            Ok(other) => warn!(?other, "unexpected GetMcpUpstreams response"),
            Err(e) => warn!(err = %e, "GetMcpUpstreams replay failed"),
        }
    }

    /// Execute one `mcp:<upstream>.<tool>` call task and emit the reply.
    async fn handle_call(&mut self, task: &Value) -> Result<()> {
        let return_route = ReturnRoute::from_task(task, local_node_id(), "agent");
        let chat_id = task
            .get("chat_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let tool_name = task
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let caller = task
            .get("agent_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let arguments = task.get("arguments").cloned().unwrap_or(json!({}));

        let outcome = self.execute_call(&tool_name, &caller, arguments).await;

        let mut reply = json!({
            "action": "datasource_response",
            "capability": "mcp.upstream.call",
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
            Ok(result) => reply["result"] = result,
            Err(message) => {
                reply["error"] = serde_json::to_value(TaskErrorPayload::provider_failure(
                    ROLE,
                    Some("mcp.upstream.call"),
                    Some(ROLE),
                    message,
                ))
                .unwrap_or(Value::Null);
            }
        }

        self.ipc_client
            .send_request_with_timeout(
                IpcRequest::EmitTask {
                    target_node: return_route.node.clone(),
                    target_role: return_route.role.clone(),
                    target_guest_id: return_route.guest_id.clone(),
                    task_json: reply.to_string(),
                },
                Duration::from_secs(10),
            )
            .await?;
        Ok(())
    }

    async fn execute_call(
        &mut self,
        tool_name: &str,
        caller: &str,
        arguments: Value,
    ) -> Result<Value, String> {
        let Some((upstream_id, remote_name)) = parse_projected_tool_name(tool_name) else {
            return Err(format!("'{tool_name}' is not an mcp:<upstream>.<tool> name"));
        };
        let (upstream_id, remote_name) = (upstream_id.to_string(), remote_name.to_string());

        let Some(client) = self.upstreams.get(&upstream_id) else {
            return Err(format!(
                "upstream '{upstream_id}' is not connected on this hotel"
            ));
        };

        // Caller must be the owner or an explicitly granted agent.
        let config = &client.config;
        if config.owner_agent_id != caller && !config.grant_agents.iter().any(|a| a == caller) {
            return Err(format!(
                "agent '{caller}' is not granted access to upstream '{upstream_id}'"
            ));
        }
        let Some(grant) = config
            .tool_allowlist
            .iter()
            .find(|g| g.remote_name == remote_name)
            .cloned()
        else {
            return Err(format!(
                "tool '{remote_name}' is not allowlisted on upstream '{upstream_id}'"
            ));
        };

        if !self
            .allotments
            .charge(&upstream_id, &remote_name, grant.allotment)
        {
            return Err(format!(
                "allotment exhausted for {upstream_id}.{remote_name} (budget {:?}/hour)",
                grant.allotment
            ));
        }

        let client = self
            .upstreams
            .get_mut(&upstream_id)
            .expect("checked above");
        client
            .call_tool(&grant, arguments)
            .await
            .map_err(|e| format!("{e:#}"))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "membrane_mcp_client=info".into()),
        )
        .init();

    let guest_id =
        std::env::var("PHILOTIC_GUEST_ID").unwrap_or_else(|_| "mcp-client".to_string());
    info!(guest_id, role = ROLE, "membrane-mcp-client starting");

    let identity = GuestIdentity {
        guest_id,
        role: ROLE.into(),
        supported_tools: Vec::new(),
    };
    let mut ipc_client = PhiloticClient::connect(identity).await?;
    ipc_client
        .send_request(IpcRequest::SubscribeInbox { role: ROLE.into() })
        .await?;

    let mut guest = Guest {
        ipc_client,
        upstreams: HashMap::new(),
        allotments: Allotments::default(),
        last_refresh: HashMap::new(),
    };
    guest.replay_upstreams().await;

    info!(role = ROLE, "listening for MCP upstream tasks");
    loop {
        match tokio::time::timeout(Duration::from_secs(5), guest.ipc_client.recv_task()).await {
            Ok(Ok(IpcResponse::InboundTask { task_json, .. })) => {
                let task: Value = match serde_json::from_str(&task_json) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(err = %e, "unparseable inbound task; skipping");
                        continue;
                    }
                };
                let action = task
                    .get("action")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                match action.as_str() {
                    "update_mcp_upstream" => {
                        match task
                            .get("config")
                            .and_then(|c| serde_json::from_value(c.clone()).ok())
                        {
                            Some(config) => guest.sync_upstream(config).await,
                            None => warn!("update_mcp_upstream push missing config"),
                        }
                    }
                    "revoke_mcp_upstream" => {
                        if let Some(id) = task.get("upstream_id").and_then(Value::as_str) {
                            guest.upstreams.remove(id);
                            info!(upstream = id, "upstream revoked");
                        }
                    }
                    "execute_tool" => {
                        if let Err(e) = guest.handle_call(&task).await {
                            warn!(err = %e, "mcp upstream call reply failed");
                        }
                    }
                    other => {
                        info!(action = other, "ignoring unrelated inbox task");
                    }
                }
            }
            Ok(Ok(other)) => {
                if matches!(other, IpcResponse::GracefulShutdown { .. }) {
                    info!("graceful shutdown push received; exiting");
                    return Ok(());
                }
            }
            Ok(Err(e)) => {
                if is_ipc_disconnect(&e) {
                    info!("hotel IPC disconnected; exiting for supervisor respawn");
                    return Ok(());
                }
                warn!(err = %e, "IPC receive error");
            }
            Err(_) => {
                guest.run_due_refreshes().await;
            }
        }
    }
}
