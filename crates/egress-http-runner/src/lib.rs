//! Bounded HTTP execution for Philotic integration bindings.
//!
//! The executor accepts a complete non-secret binding plus one request. It
//! revalidates every policy edge immediately before I/O, pins DNS to an
//! address in the declared network scope, disables ambient proxy/redirect
//! behavior, injects a caller-inaccessible credential, and returns a bounded,
//! sanitized response with content-free audit evidence.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ansible_mesh_core::integration::{
    forbidden_caller_header, ip_matches_scope, EgressPlacementDecision, HttpIntegrationAudit,
    HttpIntegrationRequest, HttpIntegrationResponse, HttpIntegrationTarget, IntegrationBinding,
    IntegrationTarget,
};
use anyhow::{anyhow, bail, Context, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, LOCATION};
use reqwest::{Method, StatusCode, Url};
use tokio::net::lookup_host;
use uuid::Uuid;

pub struct ExecutionContext<'a> {
    pub executor_node_id: &'a str,
    pub placement: EgressPlacementDecision,
    pub credential: Option<&'a str>,
    pub tool_name: &'a str,
    pub agent_id: &'a str,
    pub caller_role: &'a str,
    pub session_id: &'a str,
    pub turn_id: &'a str,
    pub correlation_id: &'a str,
}

pub async fn execute(
    binding: &IntegrationBinding,
    request: &HttpIntegrationRequest,
    context: ExecutionContext<'_>,
) -> Result<HttpIntegrationResponse> {
    let started_at_ms = unix_ms();
    let request_id = if context.correlation_id.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        context.correlation_id.to_string()
    };
    binding.validate().map_err(|message| anyhow!(message))?;
    if !binding.enabled {
        bail!("integration binding '{}' is disabled", binding.binding_id);
    }
    if request.binding_id != binding.binding_id {
        bail!(
            "request binding '{}' does not match authority '{}'",
            request.binding_id,
            binding.binding_id
        );
    }
    let IntegrationTarget::Http(target) = &binding.target else {
        bail!(
            "binding '{}' is not an HTTP integration",
            binding.binding_id
        );
    };

    let method = parse_method(&request.method)?;
    if !target.method_allowed(method.as_str()) {
        bail!(
            "method '{}' is outside binding '{}' authority",
            method,
            binding.binding_id
        );
    }
    let body = encode_body(request.body.as_ref())?;
    if body.len() as u64 > target.max_request_bytes {
        bail!(
            "request body is {} bytes; binding limit is {}",
            body.len(),
            target.max_request_bytes
        );
    }
    let mut url = build_initial_url(target, request)?;
    let base = Url::parse(&target.base_url).context("invalid binding base_url")?;
    let base_host = normalized_host(&base)?;
    let mut redirect_count = 0u8;
    let mut current_method = method;
    let mut current_body = body;

    loop {
        validate_url_authority(target, &base, &url, redirect_count > 0)?;
        let host = normalized_host(&url)?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| anyhow!("URL has no known port"))?;
        let resolved = resolve_allowed(&host, port, target).await?;
        let client = pinned_client(&host, resolved, target.timeout_secs)?;
        let mut headers = build_headers(target, request, host == base_host)?;
        let credential_injected = if host == base_host {
            inject_credential(target, &mut headers, context.credential)?
        } else {
            false
        };

        let mut builder = client
            .request(current_method.clone(), url.clone())
            .headers(headers);
        if !current_body.is_empty()
            && current_method != Method::GET
            && current_method != Method::HEAD
        {
            builder = builder
                .header("content-type", "application/json")
                .body(current_body.clone());
        }
        let response = builder
            .send()
            .await
            .context("bounded HTTP request failed")?;
        if is_redirect(response.status()) {
            if redirect_count >= target.max_redirects {
                bail!("redirect limit {} exceeded", target.max_redirects);
            }
            let location = response
                .headers()
                .get(LOCATION)
                .ok_or_else(|| anyhow!("redirect response omitted Location"))?
                .to_str()
                .context("redirect Location is not valid text")?;
            url = url.join(location).context("invalid redirect Location")?;
            redirect_count = redirect_count.saturating_add(1);
            if response.status() == StatusCode::SEE_OTHER {
                current_method = Method::GET;
                current_body.clear();
            }
            continue;
        }

        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let response_headers = sanitize_response_headers(target, response.headers());
        let bytes = read_bounded(response, target.max_response_bytes).await?;
        let response_bytes = bytes.len() as u64;
        let body = String::from_utf8(bytes)
            .map_err(|_| anyhow!("HTTP response body is not valid UTF-8"))?;
        let finished_at_ms = unix_ms();

        return Ok(HttpIntegrationResponse {
            request_id: request_id.clone(),
            status,
            headers: response_headers,
            body,
            content_type,
            response_bytes,
            audit: HttpIntegrationAudit {
                binding_id: binding.binding_id.clone(),
                tool_name: context.tool_name.to_string(),
                agent_id: context.agent_id.to_string(),
                caller_role: context.caller_role.to_string(),
                session_id: context.session_id.to_string(),
                turn_id: context.turn_id.to_string(),
                correlation_id: request_id,
                traffic_class: binding.traffic_class,
                executor_node_id: context.executor_node_id.to_string(),
                placement: context.placement,
                target_origin: origin(&url)?,
                method: current_method.to_string(),
                path: url.path().to_string(),
                policy_revision: binding.updated_at,
                approval_required: binding.requires_approval,
                credential_ref: target
                    .credential
                    .as_ref()
                    .map(|credential| credential.secret_ref.clone()),
                credential_injected,
                redirect_count,
                request_bytes: current_body.len() as u64,
                response_status: Some(status),
                response_bytes,
                started_at_ms,
                finished_at_ms,
                duration_ms: finished_at_ms.saturating_sub(started_at_ms),
                outcome: format!("http_{status}"),
                failure_code: None,
            },
        });
    }
}

fn parse_method(value: &str) -> Result<Method> {
    if value != value.to_ascii_uppercase() {
        bail!("HTTP method must be uppercase");
    }
    Method::from_bytes(value.as_bytes()).context("invalid HTTP method")
}

fn encode_body(value: Option<&serde_json::Value>) -> Result<Vec<u8>> {
    value
        .map(serde_json::to_vec)
        .transpose()
        .context("failed to encode request body")
        .map(Option::unwrap_or_default)
}

fn build_initial_url(
    target: &HttpIntegrationTarget,
    request: &HttpIntegrationRequest,
) -> Result<Url> {
    let mut url = Url::parse(&target.base_url).context("invalid binding base_url")?;
    if !request.path.is_empty() {
        if !request.path.starts_with('/')
            || request.path.contains("..")
            || request.path.contains('\\')
            || request.path.contains(['?', '#'])
        {
            bail!("request path must be absolute and cannot contain traversal, query, or fragment");
        }
        url.set_path(&request.path);
    }
    if !target.path_allowed(url.path()) {
        bail!(
            "path '{}' is outside the binding's allowed prefixes",
            url.path()
        );
    }
    if !request.query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in &request.query {
            pairs.append_pair(key, value);
        }
    }
    Ok(url)
}

fn validate_url_authority(
    target: &HttpIntegrationTarget,
    base: &Url,
    url: &Url,
    redirected: bool,
) -> Result<()> {
    if !matches!(url.scheme(), "http" | "https") {
        bail!("only HTTP(S) URLs may cross the egress boundary");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("URL userinfo is forbidden");
    }
    let host = normalized_host(url)?;
    let base_host = normalized_host(base)?;
    if redirected
        && host != base_host
        && !target
            .allowed_redirect_hosts
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(&host))
    {
        bail!("redirect host '{host}' is outside binding authority");
    }
    if !redirected
        && (host != base_host || url.port_or_known_default() != base.port_or_known_default())
    {
        bail!("request origin differs from binding base_url");
    }
    Ok(())
}

async fn resolve_allowed(
    host: &str,
    port: u16,
    target: &HttpIntegrationTarget,
) -> Result<SocketAddr> {
    let addresses: Vec<SocketAddr> = lookup_host((host, port))
        .await
        .with_context(|| format!("DNS resolution failed for '{host}'"))?
        .collect();
    if addresses.is_empty() {
        bail!("DNS resolution returned no addresses for '{host}'");
    }
    addresses
        .into_iter()
        .find(|address| ip_matches_scope(address.ip(), target.network_scope))
        .ok_or_else(|| {
            anyhow!(
                "all resolved addresses for '{}' are outside {:?} scope",
                host,
                target.network_scope
            )
        })
}

fn pinned_client(host: &str, address: SocketAddr, timeout_secs: u64) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(timeout_secs));
    if host.parse::<IpAddr>().is_err() {
        builder = builder.resolve(host, address);
    }
    builder
        .build()
        .context("failed to build bounded HTTP client")
}

fn build_headers(
    target: &HttpIntegrationTarget,
    request: &HttpIntegrationRequest,
    is_base_host: bool,
) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    for (name, value) in &target.default_headers {
        if !is_base_host {
            continue;
        }
        insert_header(&mut headers, name, value)?;
    }
    for (name, value) in &request.headers {
        if forbidden_caller_header(name) {
            bail!("caller-controlled header '{name}' is forbidden");
        }
        if !target
            .allowed_request_headers
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(name))
        {
            bail!("caller-controlled header '{name}' is outside binding authority");
        }
        insert_header(&mut headers, name, value)?;
    }
    Ok(headers)
}

fn inject_credential(
    target: &HttpIntegrationTarget,
    headers: &mut HeaderMap,
    credential: Option<&str>,
) -> Result<bool> {
    let Some(binding) = &target.credential else {
        return Ok(false);
    };
    let secret = credential.ok_or_else(|| {
        anyhow!(
            "binding requires credential ref '{}' but the executor did not receive a resolved value",
            binding.secret_ref
        )
    })?;
    let value = binding.format.replacen("{}", secret, 1);
    insert_header(headers, &binding.header, &value)?;
    Ok(true)
}

fn insert_header(headers: &mut HeaderMap, name: &str, value: &str) -> Result<()> {
    let name = HeaderName::from_bytes(name.as_bytes()).context("invalid HTTP header name")?;
    let value = HeaderValue::from_str(value).context("invalid HTTP header value")?;
    headers.insert(name, value);
    Ok(())
}

fn sanitize_response_headers(
    target: &HttpIntegrationTarget,
    source: &HeaderMap,
) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    for allowed in &target.response_header_allowlist {
        if let Some(value) = source.get(allowed).and_then(|value| value.to_str().ok()) {
            result.insert(allowed.to_ascii_lowercase(), value.to_string());
        }
    }
    result
}

async fn read_bounded(mut response: reqwest::Response, limit: u64) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        bail!("response Content-Length exceeds binding limit of {limit} bytes");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("failed reading response body")?
    {
        if body.len() as u64 + chunk.len() as u64 > limit {
            bail!("response body exceeds binding limit of {limit} bytes");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn normalized_host(url: &Url) -> Result<String> {
    url.host_str()
        .map(|host| host.trim_matches(['[', ']']).to_ascii_lowercase())
        .ok_or_else(|| anyhow!("URL has no host"))
}

fn origin(url: &Url) -> Result<String> {
    let host = normalized_host(url)?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow!("URL has no known port"))?;
    Ok(format!("{}://{}:{}", url.scheme(), host, port))
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::integration::{
        EgressPlacementPolicy, EgressTrafficClass, HttpCredentialBinding, HttpNetworkScope,
    };
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn one_shot_server(
        response_body: &'static str,
    ) -> (SocketAddr, tokio::task::JoinHandle<String>) {
        raw_one_shot_server(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Hidden: no\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        ))
        .await
    }

    async fn raw_one_shot_server(
        response: String,
    ) -> (SocketAddr, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let count = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..count]).to_string();
            stream.write_all(response.as_bytes()).await.unwrap();
            request
        });
        (address, task)
    }

    fn binding(address: SocketAddr) -> IntegrationBinding {
        IntegrationBinding {
            binding_id: "weather".into(),
            owner_agent_id: "agent-jane".into(),
            display_name: None,
            target: IntegrationTarget::Http(HttpIntegrationTarget {
                base_url: format!("http://{address}/v1"),
                allowed_methods: vec!["POST".into()],
                allowed_path_prefixes: vec!["/v1/forecast".into()],
                allowed_request_headers: vec!["x-request-tag".into()],
                default_headers: BTreeMap::from([("accept".into(), "application/json".into())]),
                response_header_allowlist: vec!["content-type".into()],
                allowed_redirect_hosts: vec![],
                network_scope: HttpNetworkScope::Loopback,
                credential: Some(HttpCredentialBinding {
                    secret_ref: "vault/weather".into(),
                    header: "authorization".into(),
                    format: "Bearer {}".into(),
                }),
                timeout_secs: 5,
                max_request_bytes: 1024,
                max_response_bytes: 1024,
                max_redirects: 0,
            }),
            grant_agents: vec![],
            grant_skills: vec![],
            traffic_class: EgressTrafficClass::GeneralApi,
            placement: EgressPlacementPolicy::Local,
            requires_approval: true,
            enabled: true,
            updated_at: 1,
        }
    }

    #[tokio::test]
    async fn bounded_call_injects_secret_and_sanitizes_response() {
        let (address, server) = one_shot_server(r#"{"temperature":72}"#).await;
        let binding = binding(address);
        let request = HttpIntegrationRequest {
            binding_id: "weather".into(),
            method: "POST".into(),
            path: "/v1/forecast".into(),
            query: BTreeMap::from([("zip".into(), "30309".into())]),
            headers: BTreeMap::from([("x-request-tag".into(), "turn-1".into())]),
            body: Some(json!({"units": "f"})),
        };
        let response = execute(
            &binding,
            &request,
            ExecutionContext {
                executor_node_id: "mbp-jane",
                placement: EgressPlacementDecision::ExecuteLocal {
                    audit_fallback: false,
                },
                credential: Some("test-secret"),
                tool_name: "http:weather.request",
                agent_id: "agent-jane",
                caller_role: "orchestrator",
                session_id: "session-1",
                turn_id: "turn-1",
                correlation_id: "request-1",
            },
        )
        .await
        .unwrap();
        let wire_request = server.await.unwrap().to_ascii_lowercase();

        assert_eq!(response.status, 200);
        assert_eq!(response.body, r#"{"temperature":72}"#);
        assert_eq!(
            response.headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
        assert!(!response.headers.contains_key("x-hidden"));
        assert!(wire_request.contains("authorization: bearer test-secret"));
        assert!(wire_request.contains("x-request-tag: turn-1"));
        assert!(wire_request.contains("post /v1/forecast?zip=30309"));
        assert!(response.audit.credential_injected);
    }

    #[tokio::test]
    async fn rejects_path_method_header_and_network_scope_before_io() {
        let (address, _server) = one_shot_server("{}").await;
        let binding = binding(address);
        let mut request = HttpIntegrationRequest {
            binding_id: "weather".into(),
            method: "GET".into(),
            path: "/v1/forecast".into(),
            query: BTreeMap::new(),
            headers: BTreeMap::new(),
            body: None,
        };
        let context = || ExecutionContext {
            executor_node_id: "mbp-jane",
            placement: EgressPlacementDecision::ExecuteLocal {
                audit_fallback: false,
            },
            credential: Some("test-secret"),
            tool_name: "http:weather.request",
            agent_id: "agent-jane",
            caller_role: "orchestrator",
            session_id: "session-1",
            turn_id: "turn-1",
            correlation_id: "request-1",
        };
        assert!(execute(&binding, &request, context())
            .await
            .unwrap_err()
            .to_string()
            .contains("method"));

        request.method = "POST".into();
        request.path = "/admin".into();
        assert!(execute(&binding, &request, context())
            .await
            .unwrap_err()
            .to_string()
            .contains("path"));

        request.path = "/v1/forecast".into();
        request
            .headers
            .insert("authorization".into(), "mine".into());
        assert!(execute(&binding, &request, context())
            .await
            .unwrap_err()
            .to_string()
            .contains("forbidden"));

        request.headers.clear();
        let mut public_binding = binding;
        let IntegrationTarget::Http(target) = &mut public_binding.target else {
            unreachable!()
        };
        target.network_scope = HttpNetworkScope::Public;
        assert!(execute(&public_binding, &request, context())
            .await
            .unwrap_err()
            .to_string()
            .contains("scope"));
    }

    #[tokio::test]
    async fn rejects_oversize_response_and_redirect_outside_binding_authority() {
        let (address, server) = one_shot_server("response-is-too-large").await;
        let mut limited_binding = binding(address);
        let IntegrationTarget::Http(target) = &mut limited_binding.target else {
            unreachable!()
        };
        target.max_response_bytes = 8;
        let request = HttpIntegrationRequest {
            binding_id: "weather".into(),
            method: "POST".into(),
            path: "/v1/forecast".into(),
            query: BTreeMap::new(),
            headers: BTreeMap::new(),
            body: None,
        };
        let error = execute(
            &limited_binding,
            &request,
            ExecutionContext {
                executor_node_id: "mbp-jane",
                placement: EgressPlacementDecision::ExecuteLocal {
                    audit_fallback: false,
                },
                credential: Some("test-secret"),
                tool_name: "http:weather.request",
                agent_id: "agent-jane",
                caller_role: "orchestrator",
                session_id: "session-1",
                turn_id: "turn-1",
                correlation_id: "request-1",
            },
        )
        .await
        .unwrap_err();
        server.await.unwrap();
        assert!(error.to_string().contains("exceeds binding limit"));

        let redirect = "HTTP/1.1 302 Found\r\nLocation: http://example.com/private\r\n\
                        Content-Length: 0\r\nConnection: close\r\n\r\n"
            .to_string();
        let (address, server) = raw_one_shot_server(redirect).await;
        let mut redirect_binding = binding(address);
        let IntegrationTarget::Http(target) = &mut redirect_binding.target else {
            unreachable!()
        };
        target.max_redirects = 1;
        let error = execute(
            &redirect_binding,
            &request,
            ExecutionContext {
                executor_node_id: "mbp-jane",
                placement: EgressPlacementDecision::ExecuteLocal {
                    audit_fallback: false,
                },
                credential: Some("test-secret"),
                tool_name: "http:weather.request",
                agent_id: "agent-jane",
                caller_role: "orchestrator",
                session_id: "session-1",
                turn_id: "turn-1",
                correlation_id: "request-1",
            },
        )
        .await
        .unwrap_err();
        server.await.unwrap();
        assert!(error.to_string().contains("redirect host"));
    }
}
