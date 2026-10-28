//! Optional recall-only resource on the existing listener. Default bearer
//! routing is untouched. OAuth tokens never enter an InboundEnvelope.
use crate::{
    auth::{CallerIdentity, extract_bearer},
    protocol::{JsonRpcRequest, JsonRpcResponse, ToolCallParams},
    server::{self, SharedState},
};
use ansible_mesh_core::{
    ExposureTier,
    mcp_endpoint::{McpEndpointConfig, McpInboundTransform},
    mcp_route::{McpAuthScheme, McpRouteTarget, McpTokenGrant},
};
use async_trait::async_trait;
use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use egress_http_runner::transient::{Admission, Claims, Refusal};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub verifier_socket: PathBuf,
    pub verifier_uid: u32,
    pub issuer: String,
    pub audience: String,
    pub client_id: String,
    pub subject: String,
    pub endpoint_id: String,
    pub existing_token_id: String,
    pub upstream_id: String,
}
#[async_trait]
pub trait Verifier: Send + Sync {
    async fn check(&self, token: &str) -> Result<Claims, Refusal>;
}
pub(crate) struct SocketVerifier {
    pub(crate) path: PathBuf,
    pub(crate) uid: u32,
}
#[async_trait]
impl Verifier for SocketVerifier {
    async fn check(&self, token: &str) -> Result<Claims, Refusal> {
        egress_http_runner::transient::introspect_at(&self.path, self.uid, token).await
    }
}
pub struct RecallResource {
    pub profile: Profile,
    pub verifier: Arc<dyn Verifier>,
    pub http_admission: Admission,
    pub verification_admission: Admission,
}
impl RecallResource {
    pub fn load(path: &Path) -> Result<Self, Refusal> {
        let p: Profile = egress_http_runner::introspection::load_owned(path)?;
        if p.issuer != "https://www.jaredlikes.com"
            || p.audience != "https://mcp.jaredlikes.com/personal/mcp"
            || p.client_id != "claude_muninn_recall_v1"
            || p.subject.is_empty()
            || p.endpoint_id.is_empty()
            || p.existing_token_id.is_empty()
            || p.upstream_id.is_empty()
            || !p.verifier_socket.is_absolute()
        {
            return Err(Refusal::Refused);
        }
        Ok(Self {
            verifier: Arc::new(SocketVerifier {
                path: p.verifier_socket.clone(),
                uid: p.verifier_uid,
            }),
            profile: p,
            http_admission: Admission::new(8, 8),
            verification_admission: Admission::new(4, 4),
        })
    }
    fn claims_valid(&self, c: &Claims, now: u64) -> bool {
        c.active
            && c.iss == self.profile.issuer
            && c.aud == self.profile.audience
            && c.client_id == self.profile.client_id
            && c.sub == self.profile.subject
            && c.scope == "memory:recall"
            && c.iat <= now
            && c.exp > now
            && c.exp.checked_sub(c.iat).is_some_and(|l| l <= 900)
    }
    pub(crate) fn grant(
        &self,
        cfg: &McpEndpointConfig,
        now: u64,
    ) -> Result<McpTokenGrant, Refusal> {
        if cfg.endpoint_id != self.profile.endpoint_id {
            return Err(Refusal::Refused);
        }
        let mut tools = cfg.tools.iter().filter(|t| t.name == "muninn_recall");
        let t = tools.next().ok_or(Refusal::Refused)?;
        if tools.next().is_some() {
            return Err(Refusal::Refused);
        }
        let McpInboundTransform::FieldMap {
            action,
            target,
            mappings,
        } = &t.inbound_transform
        else {
            return Err(Refusal::Refused);
        };
        if action != "muninn_recall" || !mappings.is_empty() || t.handler.is_some() {
            return Err(Refusal::Refused);
        }
        if !matches!(target,McpRouteTarget::McpUpstream {upstream_id,target_node:None} if upstream_id==&self.profile.upstream_id)
        {
            return Err(Refusal::Refused);
        }
        let McpAuthScheme::BearerToken { grants } = cfg.effective_auth(t) else {
            return Err(Refusal::Refused);
        };
        let mut matches = grants
            .into_iter()
            .filter(|g| g.token_id == self.profile.existing_token_id);
        let grant = matches.next().ok_or(Refusal::Refused)?;
        if matches.next().is_some() || grant.expires_at.is_some_and(|e| e <= now) {
            return Err(Refusal::Refused);
        }
        // Existing grant remains the principal and budget authority. OAuth does
        // not mint a new principal or fall back to an arbitrary token grant.
        Ok(grant)
    }
}

/// Admission runs before JSON/body extraction. Slow bodies and verifier floods
/// cannot allocate unbounded handler tasks or queue issuer calls. This is a
/// global resource limit, not authentication or a substitute for proxy limits.
pub async fn bounded_http(
    State(state): State<SharedState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let Some(resource) = &state.oauth_recall else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(_permit) = resource.http_admission.enter() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    match tokio::time::timeout(std::time::Duration::from_secs(15), next.run(request)).await {
        Ok(response) => response,
        Err(_) => StatusCode::GATEWAY_TIMEOUT.into_response(),
    }
}
pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
fn refused(id: Value) -> JsonRpcResponse {
    JsonRpcResponse::err(id, -32001, "OAuth recall refused")
}
fn challenge(reply: JsonRpcResponse) -> axum::response::Response {
    (StatusCode::UNAUTHORIZED,[("cache-control","no-store"),("www-authenticate","Bearer resource_metadata=\"https://mcp.jaredlikes.com/.well-known/oauth-protected-resource/personal/mcp\"")],axum::Json(reply)).into_response()
}
pub async fn metadata(State(state): State<SharedState>) -> axum::response::Response {
    let Some(r) = &state.oauth_recall else {
        return StatusCode::NOT_FOUND.into_response();
    };
    axum::Json(json!({"resource":r.profile.audience,"authorization_servers":[r.profile.issuer],"scopes_supported":["memory:recall"],"bearer_methods_supported":["header"]})).into_response()
}

pub async fn handle(
    State(state): State<SharedState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    axum::Json(req): axum::Json<JsonRpcRequest>,
) -> axum::response::Response {
    if state.oauth_recall.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let local = server::is_trusted_loopback(&addr, &headers);
    if let Some(denial) = server::ingress_fence_gate(&state, &headers, local) {
        if denial.status() == StatusCode::UNAUTHORIZED {
            return challenge(refused(req.id.clone().unwrap_or(Value::Null)));
        }
        return denial;
    }
    let header = headers.get("authorization").and_then(|s| s.to_str().ok());
    let notification = req.id.is_none();
    let reply = dispatch(&state, req, header).await;
    if reply.error.as_ref().is_some_and(|e| e.code == -32001) {
        return challenge(reply);
    }
    if notification {
        return StatusCode::ACCEPTED.into_response();
    }
    ([("cache-control", "no-store")], axum::Json(reply)).into_response()
}

pub(crate) async fn dispatch(
    state: &SharedState,
    req: JsonRpcRequest,
    header: Option<&str>,
) -> JsonRpcResponse {
    let id = req.id.clone().unwrap_or(Value::Null);
    let Some(resource) = &state.oauth_recall else {
        return refused(id);
    };
    let Some(_permit) = resource.verification_admission.enter() else {
        return refused(id);
    };
    if !matches!(
        req.method.as_str(),
        "initialize"
            | "ping"
            | "notifications/initialized"
            | "initialized"
            | "tools/list"
            | "tools/call"
    ) {
        return refused(id);
    }
    let Some(token) = extract_bearer(header) else {
        return refused(id);
    };
    if token.len() != 43
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return refused(id);
    }
    let initial_tier = *state.ingress_tier.read().unwrap();
    let initial_declared = *state.declared_exposure.read().unwrap();
    // Entry gate is also applied here for non-HTTP composition tests. This
    // resource grants no loopback exception and cannot widen a Local listener.
    if !perimeter_core::fence::check_ingress(initial_tier, header, false).is_allowed()
        || !matches!(
            initial_declared,
            Some(ExposureTier::Mesh | ExposureTier::Internet)
        )
    {
        return refused(id);
    }
    let Some(cfg) = state.endpoint_table.read().await.config().cloned() else {
        return refused(id);
    };
    if Some(cfg.exposure) != initial_declared || cfg.exposure > initial_tier {
        return refused(id);
    }
    let Ok(grant) = resource.grant(&cfg, now()) else {
        return refused(id);
    };
    let Ok(claims) = resource.verifier.check(token).await else {
        return refused(id);
    };
    if !resource.claims_valid(&claims, now()) {
        return refused(id);
    }
    let result = match req.method.as_str() {
        "initialize" => server::handle_initialize(id.clone(), req.params),
        "ping" => JsonRpcResponse::ok(id.clone(), json!({})),
        "notifications/initialized" | "initialized" => JsonRpcResponse::ok(id.clone(), json!({})),
        "tools/list" => JsonRpcResponse::ok(
            id.clone(),
            json!({"tools":[{"name":"muninn_recall","description":"Recall authorized memory context (read only)","inputSchema":{"type":"object","additionalProperties":false,"required":["query"],"properties":{"query":{"type":"string","minLength":1,"maxLength":4096},"limit":{"type":"integer","minimum":1,"maximum":20},"vault":{"const":"default"}}}}]}),
        ),
        "tools/call" => {
            let Some(raw) = req.params else {
                return refused(id);
            };
            if !raw
                .as_object()
                .is_some_and(|o| o.keys().all(|k| matches!(k.as_str(), "name" | "arguments")))
            {
                return refused(id);
            }
            let Ok(mut params) = serde_json::from_value::<ToolCallParams>(raw) else {
                return refused(id);
            };
            if params.name != "muninn_recall" {
                return refused(id);
            }
            let Some(mut args) = params.arguments.take().and_then(|v| v.as_object().cloned())
            else {
                return refused(id);
            };
            if args
                .keys()
                .any(|k| !matches!(k.as_str(), "query" | "limit" | "vault"))
                || !args
                    .get("query")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.trim().is_empty() && s.len() <= 4096 && !s.contains(token))
                || args
                    .get("limit")
                    .is_some_and(|v| !v.as_u64().is_some_and(|n| (1..=20).contains(&n)))
                || args
                    .get("vault")
                    .is_some_and(|v| v.as_str() != Some("default"))
            {
                return refused(id);
            }
            args.insert("vault".into(), json!("default"));
            params.arguments = Some(Value::Object(args));
            if let Some(budget) = &grant.allotment {
                if state
                    .allotment
                    .check_and_increment("muninn_recall", &grant.token_id, budget)
                    .is_err()
                {
                    return refused(id);
                }
            }
            server::handle_tools_call(
                state,
                id.clone(),
                Some(json!({"name":params.name,"arguments":params.arguments})),
                None,
                false,
                Some((
                    CallerIdentity {
                        token_id: grant.token_id.clone(),
                        scopes: grant.scopes.clone(),
                    },
                    cfg.clone(),
                )),
            )
            .await
        }
        _ => return refused(id),
    };
    // Do not release a completed recall after revocation, token expiry,
    // configuration replacement or a tightened perimeter during dispatch.
    let Ok(after) = resource.verifier.check(token).await else {
        return refused(id);
    };
    if resource.grant(&cfg, now()).is_err()
        || after != claims
        || !resource.claims_valid(&after, now())
        || state.endpoint_table.read().await.config() != Some(&cfg)
        || *state.ingress_tier.read().unwrap() != initial_tier
        || *state.declared_exposure.read().unwrap() != initial_declared
    {
        return refused(id);
    }
    result
}
