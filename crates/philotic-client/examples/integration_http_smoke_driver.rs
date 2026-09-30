use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use ansible_mesh_core::integration::{
    EgressPlacementPolicy, EgressTrafficClass, HttpCredentialBinding, HttpIntegrationRequest,
    HttpIntegrationResponse, HttpIntegrationTarget, HttpNetworkScope, IntegrationBinding,
    IntegrationTarget,
};
use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::{Duration, timeout};

const OWNER: &str = "agent-integration-smoke";
const BINDING_ID: &str = "integration-smoke";
const DRIVER_ROLE: &str = "integration-smoke-reply";
const DRIVER_GUEST_ID: &str = "integration-smoke-driver";
const CREDENTIAL: &str = "smoke-token";

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = std::env::var("PHILOTIC_HOTEL_SOCKET")
        .context("PHILOTIC_HOTEL_SOCKET must be set for integration smoke")?;
    let target_node = std::env::var("PHILOTIC_TARGET_NODE")
        .context("PHILOTIC_TARGET_NODE must be set for integration smoke")?;
    let reply_node = std::env::var("PHILOTIC_REPLY_NODE").unwrap_or_else(|_| target_node.clone());
    let exit_hotel = std::env::var("PHILOTIC_EXIT_HOTEL").ok();

    let (base_url, server) = if let Ok(base_url) = std::env::var("PHILOTIC_SMOKE_BASE_URL") {
        (base_url, None)
    } else {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let listen_addr = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            let mut request = vec![0u8; 8192];
            let count = timeout(Duration::from_secs(5), stream.read(&mut request))
                .await
                .context("timed out reading smoke HTTP request")??;
            let request = String::from_utf8_lossy(&request[..count]).to_ascii_lowercase();
            if !request.starts_with("get /v1/echo?probe=bounded http/1.1") {
                bail!("unexpected smoke request line: {request}");
            }
            if !request.contains("\r\nauthorization: bearer smoke-token\r\n") {
                bail!("runner did not inject the vault credential");
            }
            let body = r#"{"ok":true,"source":"bounded-egress"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 X-Discard-Me: secret-ish\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await?;
            stream.shutdown().await?;
            Ok::<(), anyhow::Error>(())
        });
        (format!("http://{listen_addr}/v1"), Some(server))
    };

    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: DRIVER_GUEST_ID.into(),
        role: "operator".into(),
        supported_tools: Vec::new(),
    })
    .await
    .with_context(|| format!("failed to connect integration smoke to {socket_path}"))?;
    client
        .send_request(IpcRequest::SubscribeInbox {
            role: DRIVER_ROLE.into(),
        })
        .await?;

    let binding = IntegrationBinding {
        binding_id: BINDING_ID.into(),
        owner_agent_id: OWNER.into(),
        display_name: Some("Integration smoke".into()),
        target: IntegrationTarget::Http(HttpIntegrationTarget {
            base_url,
            allowed_methods: vec!["GET".into()],
            allowed_path_prefixes: vec!["/v1/echo".into()],
            allowed_request_headers: vec![],
            default_headers: BTreeMap::new(),
            response_header_allowlist: vec!["content-type".into()],
            allowed_redirect_hosts: vec![],
            network_scope: HttpNetworkScope::Loopback,
            credential: Some(HttpCredentialBinding {
                secret_ref: format!("pending:integration/{BINDING_ID}"),
                header: "authorization".into(),
                format: "Bearer {}".into(),
            }),
            timeout_secs: 5,
            max_request_bytes: 1024,
            max_response_bytes: 4096,
            max_redirects: 0,
        }),
        grant_agents: vec![],
        grant_skills: vec!["integration.smoke".into()],
        traffic_class: EgressTrafficClass::GeneralApi,
        placement: exit_hotel
            .map(|hotel_id| EgressPlacementPolicy::RequireHotel { hotel_id })
            .unwrap_or(EgressPlacementPolicy::Local),
        requires_approval: true,
        enabled: true,
        updated_at: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    };

    match client
        .send_request(IpcRequest::RegisterIntegrationBinding {
            binding: binding.clone(),
        })
        .await?
    {
        IpcResponse::IntegrationBindingRegistered { binding_id, .. }
            if binding_id == BINDING_ID => {}
        other => bail!("unexpected binding registration response: {other:?}"),
    }
    expect_ok(
        client
            .send_request(IpcRequest::ProvisionIntegrationCredential {
                binding_id: BINDING_ID.into(),
                owner_agent_id: OWNER.into(),
                credential: CREDENTIAL.into(),
            })
            .await?,
        "credential provisioning",
    )?;

    let resolved_binding = match client
        .send_request(IpcRequest::GetIntegrationBindings {})
        .await?
    {
        IpcResponse::IntegrationBindingsState {
            integration_bindings,
        } => {
            let entry = integration_bindings
                .into_iter()
                .find(|entry| entry.binding.binding_id == BINDING_ID)
                .context("registered binding was not listed")?;
            if entry.execution_node_id.as_deref() != Some(target_node.as_str()) {
                bail!("local placement resolved to the wrong node: {entry:?}");
            }
            (entry.binding, entry.placement)
        }
        other => bail!("unexpected integration list response: {other:?}"),
    };

    expect_ok(
        client
            .send_request(IpcRequest::EmitTask {
                target_node: target_node.clone(),
                target_role: "egress-http-runner".into(),
                target_guest_id: None,
                task_json: json!({
                    "action": "execute_tool",
                    "tool_name": format!("http:{BINDING_ID}.request"),
                    "arguments": HttpIntegrationRequest {
                        binding_id: BINDING_ID.into(),
                        method: "GET".into(),
                        path: "/v1/echo".into(),
                        query: BTreeMap::from([("probe".into(), "bounded".into())]),
                        headers: BTreeMap::new(),
                        body: None,
                    },
                    "integration_binding": resolved_binding.0,
                    "integration_placement": resolved_binding.1,
                    "session_id": "smoke:integration-http",
                    "turn_id": "smoke-turn-integration-http",
                    "correlation_id": "smoke-correlation-integration-http",
                    "chat_id": "smoke-chat",
                    "agent_id": OWNER,
                    "caller_role": "integration-smoke",
                    "reply_to": reply_node,
                    "reply_role": DRIVER_ROLE,
                    "reply_guest_id": DRIVER_GUEST_ID,
                })
                .to_string(),
            })
            .await?,
        "integration task dispatch",
    )?;

    let reply = timeout(Duration::from_secs(15), async {
        loop {
            if let IpcResponse::InboundTask { task_json, .. } = client.recv_task().await? {
                break Ok::<String, anyhow::Error>(task_json);
            }
        }
    })
    .await
    .context("timed out waiting for HTTP integration response")??;
    let envelope: Value =
        serde_json::from_str(&reply).context("decode HTTP integration response envelope")?;
    if envelope.get("error").is_some() {
        bail!("HTTP integration returned an error: {}", envelope["error"]);
    }
    let result: HttpIntegrationResponse = serde_json::from_value(envelope["result"].clone())
        .context("decode bounded HTTP response")?;
    if result.status != 200 || !result.body.contains("\"bounded-egress\"") {
        bail!("unexpected bounded HTTP response: {result:?}");
    }
    if result.headers.len() != 1 || !result.headers.contains_key("content-type") {
        bail!(
            "response header allowlist was not enforced: {:?}",
            result.headers
        );
    }
    if !result.audit.credential_injected || result.audit.executor_node_id != target_node {
        bail!(
            "execution audit omitted credential or node evidence: {:?}",
            result.audit
        );
    }

    if let Some(server) = server {
        server.await.context("smoke HTTP server task panicked")??;
    }

    if target_node == reply_node {
        match client
            .send_request(IpcRequest::GetIntegrationAudit {
                binding_id: Some(BINDING_ID.into()),
                limit: Some(10),
            })
            .await?
        {
            IpcResponse::IntegrationAuditState { integration_audits }
                if integration_audits
                    .iter()
                    .any(|audit| audit.binding_id == BINDING_ID && audit.outcome == "http_200") => {
            }
            other => bail!("durable integration audit was not observable: {other:?}"),
        }
    }

    match client
        .send_request(IpcRequest::RevokeIntegrationBinding {
            binding_id: BINDING_ID.into(),
            owner_agent_id: OWNER.into(),
        })
        .await?
    {
        IpcResponse::IntegrationBindingRegistered { .. } => {}
        other => bail!("unexpected binding revoke response: {other:?}"),
    }

    println!(
        "integration HTTP smoke ok: binding -> vault credential -> bounded runner at {target_node} -> sanitized response"
    );
    Ok(())
}

fn expect_ok(response: IpcResponse, label: &str) -> Result<()> {
    match response {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        other => bail!("{label} failed: {other:?}"),
    }
}
