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

mod stdio;
mod upstream;

use ansible_mesh_core::integration::{
    projected_http_tool_name, EgressPlacementDecision, EgressTrafficClass, HttpCredentialBinding,
    HttpIntegrationRequest, HttpIntegrationResponse, HttpIntegrationTarget, HttpNetworkScope,
    IntegrationBinding, IntegrationTarget,
};
use ansible_mesh_core::mcp_upstream::{
    parse_projected_tool_name, McpUpstreamCatalog, McpUpstreamConfig, McpUpstreamTransport,
};
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use philotic_client::{
    is_ipc_disconnect, GuestIdentity, IpcRequest, IpcResponse, PhiloticClient, ReturnRoute,
    TaskErrorPayload,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};
use upstream::{HttpTransportExecutor, UpstreamClient};
use uuid::Uuid;

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
    guest_id: String,
    upstreams: HashMap<String, UpstreamClient>,
    allotments: Allotments,
    /// Unix ts of the last connect/refresh per upstream, for
    /// `refresh_interval_secs` scheduling.
    last_refresh: HashMap<String, u64>,
    deferred_tasks: Vec<Value>,
}

impl Guest {
    /// (Re)connect one upstream from a fresh config and report its catalog.
    /// A config update is the approval event, so the listing becomes the new
    /// approved baseline for stale-grant detection.
    async fn sync_upstream(&mut self, config: McpUpstreamConfig) {
        let upstream_id = config.upstream_id.clone();
        let mut client = match UpstreamClient::new(config) {
            Ok(c) => c,
            Err(e) => {
                warn!(upstream = upstream_id, err = %e, "upstream client build failed");
                return;
            }
        };
        let mut executor = RunnerHttpExecutor {
            ipc_client: &mut self.ipc_client,
            guest_id: &self.guest_id,
            deferred_tasks: &mut self.deferred_tasks,
        };
        let outcome = client.connect_and_list(&mut executor, true).await;
        self.upstreams.insert(upstream_id.clone(), client);
        self.report_outcome(&upstream_id, outcome).await;
    }

    /// Periodic re-list for one connected upstream, diffing against the
    /// approved baseline (changed tools go stale, never silently re-project).
    async fn refresh_upstream(&mut self, upstream_id: &str) {
        let Some(client) = self.upstreams.get_mut(upstream_id) else {
            return;
        };
        let mut executor = RunnerHttpExecutor {
            ipc_client: &mut self.ipc_client,
            guest_id: &self.guest_id,
            deferred_tasks: &mut self.deferred_tasks,
        };
        let outcome = client.connect_and_list(&mut executor, false).await;
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
                info!(
                    count = mcp_upstreams.len(),
                    "replaying registered upstreams"
                );
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
            return Err(format!(
                "'{tool_name}' is not an mcp:<upstream>.<tool> name"
            ));
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

        let client = self.upstreams.get_mut(&upstream_id).expect("checked above");
        let mut executor = RunnerHttpExecutor {
            ipc_client: &mut self.ipc_client,
            guest_id: &self.guest_id,
            deferred_tasks: &mut self.deferred_tasks,
        };
        client
            .call_tool(&mut executor, &grant, arguments)
            .await
            .map_err(|e| format!("{e:#}"))
    }
}

struct RunnerHttpExecutor<'a> {
    ipc_client: &'a mut PhiloticClient,
    guest_id: &'a str,
    deferred_tasks: &'a mut Vec<Value>,
}

#[async_trait]
impl HttpTransportExecutor for RunnerHttpExecutor<'_> {
    async fn post_json(&mut self, config: &McpUpstreamConfig, body: Value) -> Result<Value> {
        let McpUpstreamTransport::Http { url } = &config.transport else {
            bail!("HTTP executor called for stdio upstream");
        };
        let parsed = url::Url::parse(url).context("invalid MCP upstream URL")?;
        let host = parsed
            .host_str()
            .ok_or_else(|| anyhow!("MCP upstream URL has no host"))?;
        let path = parsed.path().to_string();
        let network_scope = config
            .http_network_scope
            .unwrap_or_else(|| infer_network_scope(host));
        let integration_id = format!("mcp:{}", config.upstream_id);

        let entries = match self
            .ipc_client
            .send_request_with_timeout(
                IpcRequest::GetIntegrationBindings {},
                Duration::from_secs(5),
            )
            .await
        {
            Ok(IpcResponse::IntegrationBindingsState {
                integration_bindings,
            }) => integration_bindings,
            Ok(other) => bail!("unexpected integration placement response: {other:?}"),
            Err(error) => return Err(error).context("fetching MCP transport placement"),
        };
        let placement_entry = entries.into_iter().find(|entry| {
            matches!(
                &entry.binding.target,
                IntegrationTarget::Mcp { upstream_id } if upstream_id == &config.upstream_id
            )
        });
        let (placement, target_node) = match placement_entry {
            Some(entry) => match (entry.placement, entry.execution_node_id) {
                (EgressPlacementDecision::Deny { reason }, _) => bail!("{reason}"),
                (placement, Some(node)) => (placement, node),
                _ => bail!("MCP transport binding has no execution node"),
            },
            None if matches!(
                config.placement,
                ansible_mesh_core::integration::EgressPlacementPolicy::Local
            ) =>
            {
                (
                    EgressPlacementDecision::ExecuteLocal {
                        audit_fallback: false,
                    },
                    local_node_id(),
                )
            }
            None => bail!(
                "MCP transport integration '{}' has not been registered",
                integration_id
            ),
        };
        let binding = IntegrationBinding {
            binding_id: integration_id.clone(),
            owner_agent_id: config.owner_agent_id.clone(),
            display_name: Some(format!("MCP transport {}", config.upstream_id)),
            target: IntegrationTarget::Http(HttpIntegrationTarget {
                base_url: url.clone(),
                allowed_methods: vec!["POST".into()],
                allowed_path_prefixes: vec![path.clone()],
                allowed_request_headers: vec![],
                default_headers: BTreeMap::from([
                    ("accept".into(), "application/json".into()),
                    ("mcp-protocol-version".into(), "2024-11-05".into()),
                ]),
                response_header_allowlist: vec!["content-type".into()],
                allowed_redirect_hosts: vec![],
                network_scope,
                credential: config.credential_ref.as_ref().map(|secret_ref| {
                    HttpCredentialBinding {
                        secret_ref: secret_ref.clone(),
                        header: "authorization".into(),
                        format: "Bearer {}".into(),
                    }
                }),
                timeout_secs: ansible_mesh_core::mcp_upstream::DEFAULT_UPSTREAM_CALL_TIMEOUT_SECS,
                max_request_bytes: 256 * 1024,
                max_response_bytes:
                    ansible_mesh_core::mcp_upstream::DEFAULT_UPSTREAM_MAX_RESPONSE_BYTES * 4,
                max_redirects: 0,
            }),
            grant_agents: config.grant_agents.clone(),
            grant_skills: vec![],
            traffic_class: EgressTrafficClass::Mcp,
            placement: config.placement.clone(),
            requires_approval: true,
            enabled: true,
            updated_at: config.updated_at.max(1),
        };
        let correlation = Uuid::new_v4().to_string();
        let request = HttpIntegrationRequest {
            binding_id: integration_id.clone(),
            method: "POST".into(),
            path,
            query: BTreeMap::new(),
            headers: BTreeMap::new(),
            body: Some(body),
        };
        let task = json!({
            "action": "execute_tool",
            "session_id": format!("mcp-transport:{}", config.upstream_id),
            "turn_id": correlation,
            "chat_id": "",
            "tool_name": projected_http_tool_name(&integration_id),
            "arguments": request,
            "execution_mode": "http_integration",
            "agent_id": config.owner_agent_id,
            "caller_role": ROLE,
            "integration_binding": binding,
            "integration_placement": placement,
            "return_route": {
                "node": local_node_id(),
                "role": ROLE,
                "guest_id": self.guest_id,
                "session_id": format!("mcp-transport:{}", config.upstream_id),
                "turn_id": correlation,
            },
            "reply_to": local_node_id(),
            "reply_role": ROLE,
            "reply_guest_id": self.guest_id,
            "final_reply_to": local_node_id(),
            "final_reply_role": ROLE,
            "final_reply_guest_id": self.guest_id,
        });
        self.ipc_client
            .send_request_with_timeout(
                IpcRequest::EmitTask {
                    target_node,
                    target_role: "egress-http-runner".into(),
                    target_guest_id: None,
                    task_json: task.to_string(),
                },
                Duration::from_secs(10),
            )
            .await
            .context("dispatching MCP HTTP envelope to egress runner")?;

        let deadline = tokio::time::Instant::now()
            + Duration::from_secs(
                ansible_mesh_core::mcp_upstream::DEFAULT_UPSTREAM_CALL_TIMEOUT_SECS + 5,
            );
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                bail!("timed out waiting for MCP HTTP egress response");
            }
            let response = tokio::time::timeout(remaining, self.ipc_client.recv_task())
                .await
                .context("timed out waiting for MCP HTTP egress response")??;
            let IpcResponse::InboundTask { task_json, .. } = response else {
                continue;
            };
            let task: Value =
                serde_json::from_str(&task_json).context("invalid egress response JSON")?;
            if task.get("turn_id").and_then(Value::as_str) != Some(correlation.as_str()) {
                self.deferred_tasks.push(task);
                continue;
            }
            if let Some(error) = task.get("error") {
                bail!("MCP HTTP egress failed: {error}");
            }
            let result: HttpIntegrationResponse = serde_json::from_value(
                task.get("result")
                    .cloned()
                    .ok_or_else(|| anyhow!("egress response omitted result"))?,
            )
            .context("invalid bounded HTTP response")?;
            if !(200..300).contains(&result.status) {
                bail!(
                    "upstream returned HTTP {}: {}",
                    result.status,
                    result.body.chars().take(512).collect::<String>()
                );
            }
            if result.body.trim().is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_str(&result.body).context("upstream response is not JSON");
        }
    }
}

fn infer_network_scope(host: &str) -> HttpNetworkScope {
    if host.eq_ignore_ascii_case("localhost") {
        return HttpNetworkScope::Loopback;
    }
    let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() else {
        return HttpNetworkScope::Public;
    };
    if ip.is_loopback() {
        return HttpNetworkScope::Loopback;
    }
    if let IpAddr::V4(v4) = ip {
        let octets = v4.octets();
        if octets[0] == 100 && (64..128).contains(&octets[1]) {
            return HttpNetworkScope::Tailnet;
        }
        if v4.is_private() || v4.is_link_local() {
            return HttpNetworkScope::Private;
        }
    }
    if let IpAddr::V6(v6) = ip {
        if v6.is_unique_local() || v6.is_unicast_link_local() {
            return HttpNetworkScope::Private;
        }
    }
    HttpNetworkScope::Public
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "membrane_mcp_client=info".into()),
        )
        .init();

    let guest_id = std::env::var("PHILOTIC_GUEST_ID").unwrap_or_else(|_| "mcp-client".to_string());
    info!(guest_id, role = ROLE, "membrane-mcp-client starting");

    let identity = GuestIdentity {
        guest_id: guest_id.clone(),
        role: ROLE.into(),
        supported_tools: Vec::new(),
    };
    let mut ipc_client = PhiloticClient::connect(identity).await?;
    ipc_client
        .send_request(IpcRequest::SubscribeInbox { role: ROLE.into() })
        .await?;

    let mut guest = Guest {
        ipc_client,
        guest_id,
        upstreams: HashMap::new(),
        allotments: Allotments::default(),
        last_refresh: HashMap::new(),
        deferred_tasks: Vec::new(),
    };
    guest.replay_upstreams().await;

    info!(role = ROLE, "listening for MCP upstream tasks");
    loop {
        let deferred = guest.deferred_tasks.pop();
        let received = match deferred {
            Some(task) => Ok(Ok(IpcResponse::InboundTask {
                source_node: local_node_id(),
                task_id: Uuid::new_v4(),
                task_json: task.to_string(),
            })),
            None => {
                tokio::time::timeout(Duration::from_secs(5), guest.ipc_client.recv_task()).await
            }
        };
        match received {
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
