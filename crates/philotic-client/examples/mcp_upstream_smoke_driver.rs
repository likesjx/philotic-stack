//! MCP upstream (client fabric) smoke driver — proposal `mcp-client-fabric`
//! Phase-1 live proof.
//!
//! Drives the full consumer loop against a running hotel and a live upstream
//! MCP server (default: the local intel-graph server on :8901):
//!
//! 1. `RegisterMcpUpstream` for the upstream with one allowlisted tool
//! 2. poll `GetMcpUpstreams` until the mcp-client guest reports `Connected`
//!    with the tool projected
//! 3. dispatch an `execute_tool` task for `mcp:<upstream>.<tool>` to the
//!    `mcp-client-runner` role (exactly what a philote's parked EmitTask
//!    dispatch sends)
//! 4. await the `datasource_response` reply on our own agent inbox
//!
//! Usage:
//!   PHILOTIC_HOTEL_SOCKET=<socket> \
//!   cargo run -p philotic-client --example mcp_upstream_smoke_driver
//!
//! Env overrides: MCP_SMOKE_URL (default http://127.0.0.1:8901/mcp),
//! MCP_SMOKE_TOOL (default graph_status), MCP_SMOKE_NODE (default node id),
//! MCP_SMOKE_CREDENTIAL (set the upstream credential via
//! ProvisionMcpUpstreamCredential before connecting — Phase-2 auth proof),
//! MCP_SMOKE_REFRESH_SECS (periodic re-list interval),
//! MCP_SMOKE_MODE=inspect (only print the stored catalog incl. stale grants,
//! no register/call — for stale-grant drills where re-registering would reset
//! the approval baseline), MCP_SMOKE_STDIO_CMD + MCP_SMOKE_STDIO_ARGS (register
//! a stdio transport instead of HTTP — space-separated args; Phase-3 proof).

use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::{Value, json};
use std::time::Duration;

const OWNER: &str = "smoke-agent";
const UPSTREAM_ID: &str = "intel-graph-smoke";

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[tokio::main]
async fn main() -> Result<()> {
    let url =
        std::env::var("MCP_SMOKE_URL").unwrap_or_else(|_| "http://127.0.0.1:8901/mcp".to_string());
    let tool = std::env::var("MCP_SMOKE_TOOL").unwrap_or_else(|_| "graph_status".to_string());
    let node_id = std::env::var("MCP_SMOKE_NODE").unwrap_or_else(|_| "local-aiua-01".to_string());
    let credential = std::env::var("MCP_SMOKE_CREDENTIAL")
        .ok()
        .filter(|s| !s.is_empty());
    let refresh_secs = std::env::var("MCP_SMOKE_REFRESH_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok());
    let inspect_only = std::env::var("MCP_SMOKE_MODE").as_deref() == Ok("inspect");
    let stdio_cmd = std::env::var("MCP_SMOKE_STDIO_CMD")
        .ok()
        .filter(|s| !s.is_empty());
    let stdio_args: Vec<String> = std::env::var("MCP_SMOKE_STDIO_ARGS")
        .ok()
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();

    let identity = GuestIdentity {
        guest_id: OWNER.into(),
        role: "agent".into(),
        supported_tools: vec![],
    };
    let mut client = PhiloticClient::connect(identity).await?;
    println!("[1/4] connected to hotel IPC");

    // Subscribe our inbox FIRST so the reply cannot race past us.
    client
        .send_request(IpcRequest::SubscribeInbox {
            role: "agent".into(),
        })
        .await?;

    if inspect_only {
        if let IpcResponse::McpUpstreamsState { mcp_upstreams } =
            client.send_request(IpcRequest::GetMcpUpstreams {}).await?
        {
            for entry in &mcp_upstreams {
                let (state, tools, stale, missing) = entry
                    .catalog
                    .as_ref()
                    .map(|c| {
                        (
                            format!("{:?}", c.state),
                            c.tools
                                .iter()
                                .map(|t| t.remote_name.clone())
                                .collect::<Vec<_>>(),
                            c.stale_grants.clone(),
                            c.missing_grants.clone(),
                        )
                    })
                    .unwrap_or_else(|| ("Pending".into(), vec![], vec![], vec![]));
                println!(
                    "upstream={} state={} projected={:?} stale={:?} missing={:?}",
                    entry.config.upstream_id, state, tools, stale, missing
                );
            }
        }
        return Ok(());
    }

    // 1. Register the upstream.
    let config = ansible_mesh_core::mcp_upstream::McpUpstreamConfig {
        upstream_id: UPSTREAM_ID.into(),
        owner_agent_id: OWNER.into(),
        transport: match &stdio_cmd {
            Some(cmd) => ansible_mesh_core::mcp_upstream::McpUpstreamTransport::Stdio {
                command: cmd.clone(),
                args: stdio_args.clone(),
            },
            None => {
                ansible_mesh_core::mcp_upstream::McpUpstreamTransport::Http { url: url.clone() }
            }
        },
        credential_ref: None,
        tool_allowlist: vec![ansible_mesh_core::mcp_upstream::McpUpstreamToolGrant {
            remote_name: tool.clone(),
            allotment: Some(10),
            max_response_bytes: None,
        }],
        grant_agents: vec![],
        refresh_interval_secs: refresh_secs,
        updated_at: now(),
    };
    match client
        .send_request(IpcRequest::RegisterMcpUpstream { config })
        .await?
    {
        IpcResponse::McpUpstreamRegistered {
            mcp_upstream_id,
            mcp_upstream_materialized,
        } => println!(
            "[2/4] upstream '{mcp_upstream_id}' registered (guest spawned: {mcp_upstream_materialized})"
        ),
        other => bail!("RegisterMcpUpstream unexpected response: {other:?}"),
    }

    // 1b. Provision the outbound credential (Phase-2 authenticated path).
    if let Some(cred) = credential {
        match client
            .send_request(IpcRequest::ProvisionMcpUpstreamCredential {
                upstream_id: UPSTREAM_ID.into(),
                owner_agent_id: OWNER.into(),
                credential: cred,
            })
            .await?
        {
            IpcResponse::Standard { ok: true, data, .. } => println!(
                "      credential stored in vault ({})",
                data.and_then(|d| d.get("vault_ref").cloned())
                    .unwrap_or_default()
            ),
            other => bail!("ProvisionMcpUpstreamCredential unexpected response: {other:?}"),
        }
    }

    // 2. Poll until the guest reports the catalog.
    let projected_name = ansible_mesh_core::mcp_upstream::projected_tool_name(UPSTREAM_ID, &tool);
    let mut connected = false;
    for attempt in 0..30 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let IpcResponse::McpUpstreamsState { mcp_upstreams } =
            client.send_request(IpcRequest::GetMcpUpstreams {}).await?
        {
            if let Some(entry) = mcp_upstreams
                .iter()
                .find(|e| e.config.upstream_id == UPSTREAM_ID)
            {
                if let Some(catalog) = &entry.catalog {
                    let has_tool = catalog.tools.iter().any(|t| t.remote_name == tool);
                    println!(
                        "      attempt {attempt}: state={:?} projected={}",
                        catalog.state, has_tool
                    );
                    if matches!(
                        catalog.state,
                        ansible_mesh_core::mcp_upstream::McpUpstreamState::Connected
                    ) && has_tool
                    {
                        connected = true;
                        break;
                    }
                }
            }
        }
    }
    if !connected {
        bail!("upstream never reported Connected with '{tool}' projected within 30s");
    }
    println!("[3/4] catalog reported: {projected_name} is projected");

    // 3. Dispatch the call exactly as philote's parked dispatch would.
    let session_id = format!("smoke-mcp-{}", now());
    let turn_id = format!("turn-{}", now());
    let task = json!({
        "action": "execute_tool",
        "session_id": session_id,
        "turn_id": turn_id,
        "chat_id": "smoke",
        "tool_name": projected_name,
        "arguments": {},
        "execution_mode": "mcp_upstream",
        "agent_id": OWNER,
        "return_route": {
            "node": node_id,
            "role": "agent",
            "guest_id": OWNER,
            "session_id": session_id,
            "turn_id": turn_id,
        },
        "reply_to": node_id,
        "reply_role": "agent",
        "reply_guest_id": OWNER,
    });
    client
        .send_request(IpcRequest::EmitTask {
            target_node: node_id.clone(),
            target_role: "mcp-client-runner".into(),
            target_guest_id: None,
            task_json: task.to_string(),
        })
        .await
        .context("EmitTask dispatch failed")?;
    println!("      call dispatched, awaiting datasource_response…");

    // 4. Await the reply on our inbox.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            bail!("no datasource_response within 45s");
        }
        match tokio::time::timeout(remaining, client.recv_task()).await {
            Ok(Ok(IpcResponse::InboundTask { task_json, .. })) => {
                let payload: Value = serde_json::from_str(&task_json).unwrap_or_default();
                if payload.get("action").and_then(Value::as_str) != Some("datasource_response") {
                    continue;
                }
                if payload.get("turn_id").and_then(Value::as_str) != Some(turn_id.as_str()) {
                    continue;
                }
                if let Some(err) = payload.get("error") {
                    bail!("remote call returned error: {err}");
                }
                let result = payload.get("result").cloned().unwrap_or(Value::Null);
                let rendered = serde_json::to_string(&result).unwrap_or_default();
                println!(
                    "[4/4] datasource_response received ({} bytes): {}",
                    rendered.len(),
                    &rendered[..rendered.len().min(400)]
                );
                println!("SMOKE-GREEN: mcp-client-fabric Phase-1 loop proven end to end");
                return Ok(());
            }
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => bail!("IPC receive error: {e}"),
            Err(_) => bail!("no datasource_response within 45s"),
        }
    }
}
