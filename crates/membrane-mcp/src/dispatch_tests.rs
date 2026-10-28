//! Dispatch-surface tests: fence tiers × auth schemes × source addresses ×
//! bind modes, the oneshot correlation path, and error mapping — all against
//! an in-memory runtime stub, no real sockets.

use crate::auth::{AllotmentTracker, VaultHashCache, VaultResolver};
use crate::protocol::JsonRpcRequest;
use crate::routing::{new_shared_endpoint_table, new_shared_table};
use crate::server::{DispatchOutcome, MembraneState, SharedState, dispatch_rpc};
use crate::{ListenerManager, McpMembrane};
use ansible_mesh_core::ExposureTier;
use ansible_mesh_core::mcp_endpoint::{
    FieldMapping, McpEndpointConfig, McpInboundTransform, McpOutboundTransform, McpPreapprovalRule,
    McpToolSpec,
};
use ansible_mesh_core::mcp_route::{
    McpAuthScheme, McpCallAllotment, McpRouteTarget, McpTokenGrant,
};
use membrane::MembraneGuest;
use membrane::envelope::{InboundEnvelope, OutboundReply};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc, oneshot};

// ── Harness ───────────────────────────────────────────────────────────────────

struct MapVault(HashMap<String, [u8; 32]>);

impl VaultResolver for MapVault {
    fn resolve(&self, vault_ref: &str) -> anyhow::Result<[u8; 32]> {
        self.0
            .get(vault_ref)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("not found: {}", vault_ref))
    }
}

struct Harness {
    state: SharedState,
    inbound_rx: Option<mpsc::Receiver<InboundEnvelope>>,
}

fn harness(static_mode: bool, tier: ExposureTier, vault: HashMap<String, [u8; 32]>) -> Harness {
    let (inbound_tx, inbound_rx) = mpsc::channel(16);
    let state = Arc::new(MembraneState {
        oauth_recall: None,
        routing_table: new_shared_table(),
        endpoint_table: new_shared_endpoint_table(),
        vault_cache: VaultHashCache::new(),
        allotment: AllotmentTracker::new(),
        vault: Box::new(MapVault(vault)),
        node_id: "test-node".into(),
        guest_id: "mcp-membrane-test".into(),
        inbound_tx,
        pending_responses: Arc::new(Mutex::new(HashMap::new())),
        streaming_buffers: Arc::new(Mutex::new(HashMap::new())),
        static_mode,
        ingress_tier: Arc::new(std::sync::RwLock::new(tier)),
        declared_exposure: Arc::new(std::sync::RwLock::new(None)),
    });
    Harness {
        state,
        inbound_rx: Some(inbound_rx),
    }
}

impl Harness {
    /// Spawn a stub philote runtime that resolves every dispatched envelope
    /// with the given outcome.
    fn spawn_stub(
        &mut self,
        outcome: impl Fn(&InboundEnvelope) -> DispatchOutcome + Send + 'static,
    ) {
        let mut rx = self.inbound_rx.take().expect("stub already spawned");
        let state = self.state.clone();
        tokio::spawn(async move {
            while let Some(env) = rx.recv().await {
                let tx = { state.pending_responses.lock().await.remove(&env.turn_id) };
                if let Some(tx) = tx {
                    let _ = tx.send(outcome(&env));
                }
            }
        });
    }

    async fn set_endpoint(&self, config: McpEndpointConfig) {
        *self.state.declared_exposure.write().unwrap() = Some(config.exposure);
        self.state.endpoint_table.write().await.update(config);
    }
}

fn rpc(method: &str, params: Value) -> JsonRpcRequest {
    JsonRpcRequest {
        jsonrpc: "2.0".into(),
        method: method.into(),
        params: Some(params),
        id: Some(json!(1)),
    }
}

#[tokio::test(start_paused = true)]
async fn oauth_http_limits_slow_bodies_before_extraction() {
    use axum::{
        body::Body,
        extract::ConnectInfo,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let h = oauth_harness(0, None);
    let app = crate::server::build_router(h.state.clone());
    let mut request = Request::post("/personal/mcp")
        .header("content-type", "application/json")
        .body(Body::from_stream(futures_util::stream::pending::<
            Result<Vec<u8>, std::io::Error>,
        >()))
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
    assert_eq!(
        app.oneshot(request).await.unwrap().status(),
        StatusCode::GATEWAY_TIMEOUT
    );
    assert!(h.state.pending_responses.lock().await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn oauth_http_deadline_and_client_cancellation_release_turn_state() {
    use axum::{
        body::Body,
        extract::ConnectInfo,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    for cancel in [false, true] {
        let mut h = oauth_harness(0, None);
        let app = crate::server::build_router(h.state.clone());
        let mut request = Request::post("/personal/mcp").header("content-type", "application/json")
            .header("authorization", format!("Bearer {SYNTHETIC_OAUTH}"))
            .body(Body::from(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"muninn_recall","arguments":{"query":"synthetic-only"}}}).to_string())).unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
        let task = tokio::spawn(app.oneshot(request));
        let envelope = h.inbound_rx.as_mut().unwrap().recv().await.unwrap();
        h.state
            .streaming_buffers
            .lock()
            .await
            .insert(envelope.turn_id.clone(), "synthetic-partial".into());
        if cancel {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            tokio::time::advance(Duration::from_secs(16)).await;
            assert_eq!(
                task.await.unwrap().unwrap().status(),
                StatusCode::GATEWAY_TIMEOUT
            );
        }
        tokio::task::yield_now().await;
        assert!(h.state.pending_responses.lock().await.is_empty());
        assert!(h.state.streaming_buffers.lock().await.is_empty());
        assert!(
            h.state
                .oauth_recall
                .as_ref()
                .unwrap()
                .http_admission
                .enter()
                .is_some()
        );
        assert!(
            h.state
                .oauth_recall
                .as_ref()
                .unwrap()
                .verification_admission
                .enter()
                .is_some()
        );
    }
}

#[tokio::test]
async fn oauth_http_oversize_and_flood_leave_health_available() {
    use axum::{
        body::Body,
        extract::ConnectInfo,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let h = oauth_harness(0, None);
    let app = crate::server::build_router(h.state.clone());
    let mut large = Request::post("/personal/mcp")
        .header("content-type", "application/json")
        .body(Body::from("x".repeat(8193)))
        .unwrap();
    large
        .extensions_mut()
        .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
    assert_eq!(
        app.clone().oneshot(large).await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let resource = h.state.oauth_recall.as_ref().unwrap();
    let held: Vec<_> = (0..8).map(|_| resource.http_admission.enter()).collect();
    for _ in 0..50 {
        let req = Request::post("/personal/mcp").body(Body::empty()).unwrap();
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }
    let mut health = Request::get("/health").body(Body::empty()).unwrap();
    health
        .extensions_mut()
        .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
    assert_eq!(app.oneshot(health).await.unwrap().status(), StatusCode::OK);
    drop(held);
}

async fn call(
    state: &SharedState,
    req: JsonRpcRequest,
    auth: Option<&str>,
    is_loopback: bool,
) -> (u16, Value) {
    let resp = dispatch_rpc(state, req, auth, is_loopback).await;
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn echo_tool(name: &str, auth: Option<McpAuthScheme>) -> McpToolSpec {
    McpToolSpec {
        name: name.into(),
        description: "echo test tool".into(),
        input_schema: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        inbound_transform: McpInboundTransform::FieldMap {
            action: "echo".into(),
            target: McpRouteTarget::Philote {
                agent_id: "agent-test".into(),
                target_node: None,
            },
            mappings: vec![FieldMapping {
                from: "q".into(),
                to: "payload.q".into(),
            }],
        },
        outbound_transform: McpOutboundTransform::PassThrough,
        auth,
        handler: None,
    }
}

fn endpoint_with(
    tools: Vec<McpToolSpec>,
    default_auth: Option<McpAuthScheme>,
) -> McpEndpointConfig {
    McpEndpointConfig {
        endpoint_id: "test-ep".into(),
        owner_agent_id: "agent-test".into(),
        port: 9100,
        path: None,
        exposure: ExposureTier::Local,
        tools,
        default_auth,
        allow_unauthenticated: false,
        preapproval_rules: vec![McpPreapprovalRule {
            action_pattern: "*".into(),
            target: None,
            approved_by_turn: "turn-test".into(),
            approved_at: 0,
            expires_at: None,
        }],
        updated_at: 0,
    }
}

fn bearer_grant(token: &str, vault: &mut HashMap<String, [u8; 32]>) -> McpTokenGrant {
    bearer_grant_opts(token, vault, None, None)
}

fn bearer_grant_opts(
    token: &str,
    vault: &mut HashMap<String, [u8; 32]>,
    expires_at: Option<u64>,
    allotment: Option<McpCallAllotment>,
) -> McpTokenGrant {
    let vault_ref = format!("vault/{token}");
    vault.insert(
        vault_ref.clone(),
        *blake3::hash(token.as_bytes()).as_bytes(),
    );
    McpTokenGrant {
        token_id: format!("grant-{token}"),
        vault_ref,
        scopes: vec![],
        expires_at,
        allotment,
    }
}

// OAuth uses the actual ingress dispatch/correlation path. Only the issuer
// response is synthetic; no unavailable mac-jane source is claimed as tested.
struct ClaimsVerifier {
    issued: u64,
    calls: std::sync::atomic::AtomicUsize,
    mode: u8,
    state: std::sync::Mutex<std::sync::Weak<MembraneState>>,
}
#[async_trait::async_trait]
impl crate::oauth_recall::Verifier for ClaimsVerifier {
    async fn check(
        &self,
        _: &str,
    ) -> Result<egress_http_runner::transient::Claims, egress_http_runner::transient::Refusal> {
        let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.mode == 1 {
            return Err(egress_http_runner::transient::Refusal::Unavailable);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut claims = egress_http_runner::transient::Claims {
            active: true,
            iss: "https://www.jaredlikes.com".into(),
            aud: "https://mcp.jaredlikes.com/personal/mcp".into(),
            sub: "synthetic-subject".into(),
            client_id: "claude_muninn_recall_v1".into(),
            scope: "memory:recall".into(),
            iat: self.issued - 30,
            exp: self.issued + 600,
        };
        match self.mode {
            2 => claims.sub = "outsider".into(),
            3 => claims.client_id = "chatgpt".into(),
            4 => claims.aud = "wrong-resource".into(),
            5 => claims.scope = "memory:recall memory:write".into(),
            6 => claims.exp = now - 1,
            7 => claims.iat = now + 60,
            8 if n > 0 => claims.active = false,
            9 | 10 if n > 0 => {
                let weak = self.state.lock().unwrap().clone();
                if let Some(state) = weak.upgrade() {
                    if self.mode == 9 {
                        let mut table = state.endpoint_table.write().await;
                        let mut cfg = table.config().unwrap().clone();
                        cfg.updated_at += 1;
                        table.update(cfg);
                    } else {
                        *state.ingress_tier.write().unwrap() = ExposureTier::Local;
                    }
                }
            }
            11 => claims.iss = "https://evil.invalid".into(),
            12 => {
                let weak = self.state.lock().unwrap().clone();
                if let Some(state) = weak.upgrade() {
                    state.endpoint_table.write().await.revoke();
                }
            }
            13 if n == 0 => tokio::time::sleep(Duration::from_millis(1100)).await,
            14 if n > 0 => tokio::time::sleep(Duration::from_millis(1100)).await,
            _ => {}
        }
        Ok(claims)
    }
}
fn oauth_harness(mode: u8, budget: Option<McpCallAllotment>) -> Harness {
    let mut h = harness(false, ExposureTier::Mesh, HashMap::new());
    let verifier = Arc::new(ClaimsVerifier {
        issued: crate::oauth_recall::now(),
        calls: std::sync::atomic::AtomicUsize::new(0),
        mode,
        state: std::sync::Mutex::new(std::sync::Weak::new()),
    });
    Arc::get_mut(&mut h.state).unwrap().oauth_recall =
        Some(Arc::new(crate::oauth_recall::RecallResource {
            profile: crate::oauth_recall::Profile {
                verifier_socket: "/unused/synthetic.sock".into(),
                verifier_uid: 0,
                issuer: "https://www.jaredlikes.com".into(),
                audience: "https://mcp.jaredlikes.com/personal/mcp".into(),
                client_id: "claude_muninn_recall_v1".into(),
                subject: "synthetic-subject".into(),
                endpoint_id: "test-endpoint".into(),
                existing_token_id: "existing-claude-grant".into(),
                upstream_id: "synthetic-muninn".into(),
            },
            verifier: verifier.clone(),
            http_admission: egress_http_runner::transient::Admission::new(8, 8),
            verification_admission: egress_http_runner::transient::Admission::new(4, 4),
        }));
    // Assign the weak reference after the unique-Arc initialization above.
    *verifier.state.lock().unwrap() = Arc::downgrade(&h.state);
    *h.state.declared_exposure.write().unwrap() = Some(ExposureTier::Mesh);
    let mut tool = echo_tool("muninn_recall", None);
    tool.inbound_transform = McpInboundTransform::FieldMap {
        action: "muninn_recall".into(),
        target: McpRouteTarget::McpUpstream {
            upstream_id: "synthetic-muninn".into(),
            target_node: None,
        },
        mappings: vec![],
    };
    let grant = McpTokenGrant {
        token_id: "existing-claude-grant".into(),
        vault_ref: "secret://synthetic/legacy-hash".into(),
        scopes: vec!["memory:recall".into()],
        expires_at: None,
        allotment: budget,
    };
    let mut cfg = endpoint_with(
        vec![tool],
        Some(McpAuthScheme::BearerToken {
            grants: vec![grant],
        }),
    );
    cfg.endpoint_id = "test-endpoint".into();
    cfg.exposure = ExposureTier::Mesh;
    h.state.endpoint_table.try_write().unwrap().update(cfg);
    h
}
const SYNTHETIC_OAUTH: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
async fn oauth_call(h: &Harness, method: &str, params: Value) -> Value {
    serde_json::to_value(
        crate::oauth_recall::dispatch(
            &h.state,
            rpc(method, params),
            Some(&format!("Bearer {SYNTHETIC_OAUTH}")),
        )
        .await,
    )
    .unwrap()
}
#[tokio::test]
async fn oauth_composes_existing_principal_and_dispatch_without_token_persistence() {
    let mut h = oauth_harness(0, None);
    h.spawn_stub(|e| {
        let payload = serde_json::to_value(e).unwrap();
        let text = payload.to_string();
        assert!(!text.contains(SYNTHETIC_OAUTH));
        assert_eq!(e.sender.id.as_deref(), Some("existing-claude-grant"));
        assert_eq!(e.raw_transport["target_kind"], "mcp_upstream");
        assert_eq!(e.raw_transport["target_id"], "synthetic-muninn");
        assert_eq!(e.raw_transport["args"]["vault"], "default");
        DispatchOutcome::Ok(
            json!({"content":[{"type":"text","text":"SYNTHETIC_RECALL"}]}).to_string(),
        )
    });
    let out = oauth_call(
        &h,
        "tools/call",
        json!({"name":"muninn_recall","arguments":{"query":"harmless synthetic record"}}),
    )
    .await;
    assert!(out["result"].to_string().contains("SYNTHETIC_RECALL"));
    assert!(out.get("error").is_none());
    assert!(h.state.pending_responses.lock().await.is_empty());
}
#[tokio::test]
async fn oauth_rejects_identity_scope_expiry_and_unavailable_verifier_before_dispatch() {
    for mode in [1, 2, 3, 4, 5, 6, 7, 11] {
        let h = oauth_harness(mode, None);
        let out = oauth_call(
            &h,
            "tools/call",
            json!({"name":"muninn_recall","arguments":{"query":"synthetic"}}),
        )
        .await;
        assert!(out.get("error").is_some(), "mode {mode}");
        assert!(h.state.pending_responses.lock().await.is_empty());
    }
}
#[tokio::test]
async fn oauth_withholds_results_after_revocation_endpoint_or_perimeter_change() {
    for mode in [8, 9, 10] {
        let mut h = oauth_harness(mode, None);
        h.spawn_stub(|_| DispatchOutcome::Ok("SYNTHETIC_PRIVATE_RESULT".into()));
        let out = oauth_call(
            &h,
            "tools/call",
            json!({"name":"muninn_recall","arguments":{"query":"synthetic"}}),
        )
        .await;
        assert!(out.get("error").is_some());
        assert!(!out.to_string().contains("SYNTHETIC_PRIVATE_RESULT"));
    }
}
#[tokio::test]
async fn oauth_refuses_writes_vault_target_overrides_and_token_as_query() {
    for params in [
        json!({"name":"muninn_remember","arguments":{"query":"synthetic"}}),
        json!({"name":"muninn_recall","target":"evil","arguments":{"query":"synthetic"}}),
        json!({"name":"muninn_recall","arguments":{"query":"synthetic","vault":"other"}}),
        json!({"name":"muninn_recall","arguments":{"query":"synthetic","url":"https://evil.invalid"}}),
        json!({"name":"muninn_recall","arguments":{"query":SYNTHETIC_OAUTH}}),
        json!({"name":"muninn_recall","arguments":{"query":"synthetic","limit":21}}),
    ] {
        let h = oauth_harness(0, None);
        assert!(
            oauth_call(&h, "tools/call", params)
                .await
                .get("error")
                .is_some()
        );
        assert!(h.state.pending_responses.lock().await.is_empty());
    }
}
#[tokio::test]
async fn oauth_listing_does_not_charge_existing_call_budget() {
    let mut h = oauth_harness(
        0,
        Some(McpCallAllotment {
            max_per_window: 1,
            window_secs: 3600,
        }),
    );
    h.spawn_stub(|_| DispatchOutcome::Ok("{}".into()));
    for _ in 0..2 {
        assert!(
            oauth_call(&h, "tools/list", json!({}))
                .await
                .get("result")
                .is_some()
        );
    }
    let p = json!({"name":"muninn_recall","arguments":{"query":"synthetic"}});
    assert!(
        oauth_call(&h, "tools/call", p.clone())
            .await
            .get("result")
            .is_some()
    );
    assert!(oauth_call(&h, "tools/call", p).await.get("error").is_some());
}

#[tokio::test]
async fn oauth_missing_endpoint_never_uses_legacy_route() {
    use ansible_mesh_core::mcp_route::{McpRouteRecord, McpRouteSecurity};
    let mut h = oauth_harness(12, None);
    h.state.routing_table.write().await.upsert_agent_routes(
        "legacy",
        vec![McpRouteRecord {
            agent_id: "legacy".into(),
            tool_name: "muninn_recall".into(),
            description: "legacy fallback must not run".into(),
            input_schema: json!({}),
            target: McpRouteTarget::Philote {
                agent_id: "legacy".into(),
                target_node: None,
            },
            security: McpRouteSecurity {
                auth: McpAuthScheme::None,
                global_allotment: None,
                require_approval: false,
            },
            updated_at: 1,
        }],
    );
    let out = oauth_call(
        &h,
        "tools/call",
        json!({"name":"muninn_recall","arguments":{"query":"synthetic"}}),
    )
    .await;
    assert!(out.get("error").is_some());
    assert!(h.inbound_rx.as_mut().unwrap().try_recv().is_err());
}

#[tokio::test]
async fn oauth_refuses_exposure_mismatch_or_ceiling_below_declaration() {
    for n in 0..3 {
        let h = oauth_harness(0, None);
        if n == 2 {
            *h.state.ingress_tier.write().unwrap() = ExposureTier::Local;
        } else {
            let mut table = h.state.endpoint_table.write().await;
            let mut cfg = table.config().unwrap().clone();
            cfg.exposure = if n == 0 {
                ExposureTier::Local
            } else {
                ExposureTier::Internet
            };
            table.update(cfg);
        }
        assert!(
            oauth_call(&h, "tools/list", json!({}))
                .await
                .get("error")
                .is_some()
        );
    }
}
#[tokio::test]
async fn oauth_rechecks_time_only_grant_expiry_before_dispatch_and_release() {
    for mode in [13, 14] {
        let mut h = oauth_harness(mode, None);
        {
            let mut table = h.state.endpoint_table.write().await;
            let mut cfg = table.config().unwrap().clone();
            let Some(McpAuthScheme::BearerToken { grants }) = &mut cfg.default_auth else {
                unreachable!()
            };
            grants[0].expires_at = Some(crate::oauth_recall::now() + 1);
            table.update(cfg);
        }
        if mode == 14 {
            h.spawn_stub(|_| DispatchOutcome::Ok("WITHHOLD_EXPIRED_GRANT_RESULT".into()));
        }
        let out = oauth_call(
            &h,
            "tools/call",
            json!({"name":"muninn_recall","arguments":{"query":"synthetic"}}),
        )
        .await;
        assert!(out.get("error").is_some());
        assert!(!out.to_string().contains("WITHHOLD_EXPIRED_GRANT_RESULT"));
        if mode == 13 {
            assert!(h.inbound_rx.as_mut().unwrap().try_recv().is_err());
        }
    }
}

struct PrivateFixtureAuthority {
    claims: egress_http_runner::transient::Claims,
    denied: bool,
}
#[async_trait::async_trait]
impl egress_http_runner::transient::Authority for PrivateFixtureAuthority {
    async fn introspect(
        &mut self,
        token: &str,
    ) -> Result<egress_http_runner::transient::Claims, egress_http_runner::transient::Refusal> {
        assert_eq!(token, SYNTHETIC_OAUTH);
        if self.denied {
            Err(egress_http_runner::transient::Refusal::AuditRejected)
        } else {
            Ok(self.claims.clone())
        }
    }
    fn abandon(&mut self) {}
}
#[tokio::test]
async fn oauth_composes_real_private_socket_with_ingress_and_audit_refusal() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    for denied in [false, true] {
        let dir = std::fs::canonicalize("/tmp").unwrap().join(format!(
            "oc{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let uid = std::fs::metadata(&dir).unwrap().uid();
        let path = dir.join("v.sock");
        let listener =
            egress_http_runner::transient::PrivateListener::bind(&path, uid, uid).unwrap();
        let mut h = oauth_harness(0, None);
        let mut inner = Arc::try_unwrap(h.state).ok().unwrap();
        let mut r = Arc::try_unwrap(inner.oauth_recall.take().unwrap())
            .ok()
            .unwrap();
        r.verifier = Arc::new(crate::oauth_recall::SocketVerifier {
            path: path.clone(),
            uid,
        });
        inner.oauth_recall = Some(Arc::new(r));
        h.state = Arc::new(inner);
        let now = crate::oauth_recall::now();
        let mut authority = PrivateFixtureAuthority {
            claims: egress_http_runner::transient::Claims {
                active: true,
                iss: "https://www.jaredlikes.com".into(),
                aud: "https://mcp.jaredlikes.com/personal/mcp".into(),
                sub: "synthetic-subject".into(),
                client_id: "claude_muninn_recall_v1".into(),
                scope: "memory:recall".into(),
                iat: now,
                exp: now + 600,
            },
            denied,
        };
        let server = tokio::spawn(async move {
            for _ in 0..if denied { 1 } else { 2 } {
                let mut s = listener.accept().await.unwrap();
                egress_http_runner::transient::serve(&mut s, &mut authority)
                    .await
                    .unwrap();
            }
        });
        if !denied {
            h.spawn_stub(|e| {
                assert!(!serde_json::to_string(e).unwrap().contains(SYNTHETIC_OAUTH));
                DispatchOutcome::Ok("{\"synthetic\":true}".into())
            });
        }
        let out = oauth_call(
            &h,
            "tools/call",
            json!({"name":"muninn_recall","arguments":{"query":"synthetic"}}),
        )
        .await;
        assert_eq!(out.get("error").is_some(), denied);
        if denied {
            assert!(h.inbound_rx.as_mut().unwrap().try_recv().is_err());
        }
        server.await.unwrap();
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}

// ── initialize / notifications ────────────────────────────────────────────────

#[tokio::test]
async fn initialize_echoes_supported_client_version() {
    let h = harness(false, ExposureTier::Local, HashMap::new());
    let (_, body) = call(
        &h.state,
        rpc("initialize", json!({"protocolVersion": "2024-11-05"})),
        None,
        true,
    )
    .await;
    assert_eq!(body["result"]["protocolVersion"], "2024-11-05");
}

#[tokio::test]
async fn initialize_falls_back_to_newest_supported_version() {
    let h = harness(false, ExposureTier::Local, HashMap::new());
    let (_, body) = call(
        &h.state,
        rpc("initialize", json!({"protocolVersion": "1999-01-01"})),
        None,
        true,
    )
    .await;
    assert_eq!(body["result"]["protocolVersion"], "2025-03-26");
}

#[tokio::test]
async fn notifications_initialized_gets_no_response_body() {
    let h = harness(false, ExposureTier::Local, HashMap::new());
    for method in ["notifications/initialized", "initialized"] {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params: None,
            id: None,
        };
        let (status, body) = call(&h.state, req, None, true).await;
        assert_eq!(status, 202, "{method}");
        assert_eq!(body, Value::Null, "{method}");
    }
}

// ── Ingress fence ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn fence_mesh_tier_requires_bearer_from_remote() {
    let h = harness(false, ExposureTier::Mesh, HashMap::new());
    let (status, _) = call(&h.state, rpc("ping", json!({})), None, false).await;
    assert_eq!(status, 401);

    // Structural bearer presence passes the fence (validity checked per-route).
    let (status, _) = call(
        &h.state,
        rpc("ping", json!({})),
        Some("Bearer whatever"),
        false,
    )
    .await;
    assert_eq!(status, 200);

    // Loopback callers are local hotel tooling — no bearer needed at Mesh tier.
    let (status, _) = call(&h.state, rpc("ping", json!({})), None, true).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn fence_internet_tier_has_no_loopback_bypass() {
    let h = harness(false, ExposureTier::Internet, HashMap::new());
    let (status, _) = call(&h.state, rpc("ping", json!({})), None, true).await;
    assert_eq!(status, 401);
}

// ── tools/list visibility ─────────────────────────────────────────────────────

#[tokio::test]
async fn tools_list_hides_none_auth_tools_from_remote_callers() {
    let h = harness(false, ExposureTier::Lan, HashMap::new());
    h.set_endpoint(endpoint_with(vec![echo_tool("echo", None)], None))
        .await;

    let (_, body) = call(&h.state, rpc("tools/list", json!({})), None, true).await;
    assert_eq!(body["result"]["tools"].as_array().unwrap().len(), 1);

    let (_, body) = call(&h.state, rpc("tools/list", json!({})), None, false).await;
    assert_eq!(body["result"]["tools"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn tools_list_endpoint_default_auth_governs_visibility() {
    let mut vault = HashMap::new();
    let grant = bearer_grant("tok-list", &mut vault);
    let h = harness(false, ExposureTier::Lan, vault);
    h.set_endpoint(endpoint_with(
        vec![echo_tool("echo", None)],
        Some(McpAuthScheme::BearerToken {
            grants: vec![grant],
        }),
    ))
    .await;

    // Valid bearer (inherited via default_auth) sees the tool from anywhere.
    let (_, body) = call(
        &h.state,
        rpc("tools/list", json!({})),
        Some("Bearer tok-list"),
        false,
    )
    .await;
    assert_eq!(body["result"]["tools"].as_array().unwrap().len(), 1);

    // Wrong bearer sees nothing — and default_auth also overrides the
    // loopback-open behavior None would have had.
    let (_, body) = call(
        &h.state,
        rpc("tools/list", json!({})),
        Some("Bearer wrong"),
        true,
    )
    .await;
    assert_eq!(body["result"]["tools"].as_array().unwrap().len(), 0);
}

// ── tools/call auth and error mapping ─────────────────────────────────────────

#[tokio::test]
async fn tools_call_none_auth_rejected_from_remote() {
    let h = harness(false, ExposureTier::Lan, HashMap::new());
    h.set_endpoint(endpoint_with(vec![echo_tool("echo", None)], None))
        .await;

    let (_, body) = call(
        &h.state,
        rpc(
            "tools/call",
            json!({"name": "echo", "arguments": {"q": "hi"}}),
        ),
        None,
        false,
    )
    .await;
    assert_eq!(body["error"]["code"], -32001);
}

#[tokio::test]
async fn tools_call_unknown_tool_returns_tool_not_found() {
    let h = harness(false, ExposureTier::Local, HashMap::new());
    h.set_endpoint(endpoint_with(vec![echo_tool("echo", None)], None))
        .await;

    let (_, body) = call(
        &h.state,
        rpc("tools/call", json!({"name": "nope", "arguments": {}})),
        None,
        true,
    )
    .await;
    assert_eq!(body["error"]["code"], -32000);
}

#[tokio::test]
async fn tools_call_dispatches_and_returns_result() {
    let mut h = harness(false, ExposureTier::Local, HashMap::new());
    h.set_endpoint(endpoint_with(vec![echo_tool("echo", None)], None))
        .await;
    h.spawn_stub(|env| {
        // The envelope carries the transformed action payload.
        assert!(env.content.contains("\"action\":\"echo\""));
        DispatchOutcome::Ok(json!({"answer": 42}).to_string())
    });

    let (_, body) = call(
        &h.state,
        rpc(
            "tools/call",
            json!({"name": "echo", "arguments": {"q": "hi"}}),
        ),
        None,
        true,
    )
    .await;
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("42"), "unexpected result text: {text}");
    assert!(body["result"]["isError"].is_null());
}

#[tokio::test]
async fn tools_call_forwards_handler_policy_schema_and_args_to_philote() {
    use ansible_mesh_core::mcp_endpoint::{McpHandlerFallback, McpHandlerPolicy, McpHandlerStep};

    let mut tool = echo_tool("echo", None);
    tool.handler = Some(McpHandlerPolicy {
        validate_input: true,
        steps: vec![McpHandlerStep::Static {
            result: json!({"pong": true}),
        }],
        fallback: McpHandlerFallback::Error {
            message: "deterministic only".into(),
        },
    });
    let mut h = harness(false, ExposureTier::Local, HashMap::new());
    h.set_endpoint(endpoint_with(vec![tool], None)).await;
    h.spawn_stub(|env| {
        // The philote-side ladder needs the policy, the advertised schema, and
        // the raw args exactly as the caller sent them.
        let policy = &env.raw_transport["handler"];
        assert_eq!(policy["steps"][0]["kind"], "static");
        assert_eq!(policy["fallback"]["kind"], "error");
        assert_eq!(
            env.raw_transport["input_schema"]["properties"]["q"]["type"],
            "string"
        );
        assert_eq!(env.raw_transport["args"]["q"], "hi");
        DispatchOutcome::Ok(json!({"pong": true}).to_string())
    });

    let (_, body) = call(
        &h.state,
        rpc(
            "tools/call",
            json!({"name": "echo", "arguments": {"q": "hi"}}),
        ),
        None,
        true,
    )
    .await;
    assert!(body["result"]["isError"].is_null(), "{body}");
}

#[tokio::test]
async fn tools_call_business_error_sets_is_error() {
    let mut h = harness(false, ExposureTier::Local, HashMap::new());
    h.set_endpoint(endpoint_with(vec![echo_tool("echo", None)], None))
        .await;
    h.spawn_stub(|_| DispatchOutcome::Err("tool exploded".into()));

    let (_, body) = call(
        &h.state,
        rpc(
            "tools/call",
            json!({"name": "echo", "arguments": {"q": "hi"}}),
        ),
        None,
        true,
    )
    .await;
    assert_eq!(body["result"]["isError"], true);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("tool exploded"));
    assert!(
        body["error"].is_null(),
        "business error must not be a JSON-RPC error"
    );
}

#[tokio::test]
async fn tools_call_expired_token_returns_token_expired() {
    let mut vault = HashMap::new();
    let grant = bearer_grant_opts("tok-old", &mut vault, Some(1), None);
    let h = harness(false, ExposureTier::Local, vault);
    h.set_endpoint(endpoint_with(
        vec![echo_tool(
            "echo",
            Some(McpAuthScheme::BearerToken {
                grants: vec![grant],
            }),
        )],
        None,
    ))
    .await;

    let (_, body) = call(
        &h.state,
        rpc(
            "tools/call",
            json!({"name": "echo", "arguments": {"q": "hi"}}),
        ),
        Some("Bearer tok-old"),
        true,
    )
    .await;
    assert_eq!(body["error"]["code"], -32003);
}

#[tokio::test]
async fn tools_call_allotment_exhaustion_returns_specific_code() {
    let mut vault = HashMap::new();
    let grant = bearer_grant_opts(
        "tok-budget",
        &mut vault,
        None,
        Some(McpCallAllotment {
            max_per_window: 1,
            window_secs: 3600,
        }),
    );
    let mut h = harness(false, ExposureTier::Local, vault);
    h.set_endpoint(endpoint_with(
        vec![echo_tool(
            "echo",
            Some(McpAuthScheme::BearerToken {
                grants: vec![grant],
            }),
        )],
        None,
    ))
    .await;
    h.spawn_stub(|_| DispatchOutcome::Ok("\"ok\"".into()));

    let req = || {
        rpc(
            "tools/call",
            json!({"name": "echo", "arguments": {"q": "hi"}}),
        )
    };
    let (_, first) = call(&h.state, req(), Some("Bearer tok-budget"), true).await;
    assert!(first["error"].is_null(), "first call should pass: {first}");
    let (_, second) = call(&h.state, req(), Some("Bearer tok-budget"), true).await;
    assert_eq!(second["error"]["code"], -32002);
}

#[tokio::test]
async fn static_mode_tools_call_fails_fast() {
    let h = harness(true, ExposureTier::Local, HashMap::new());
    let started = std::time::Instant::now();
    let (_, body) = call(
        &h.state,
        rpc("tools/call", json!({"name": "echo", "arguments": {}})),
        None,
        true,
    )
    .await;
    assert_eq!(body["error"]["code"], -32004);
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
}

// ── Bind derivation ───────────────────────────────────────────────────────────

#[tokio::test]
async fn bind_addr_derives_from_declared_exposure_and_ceiling() {
    let cases: &[(Option<ExposureTier>, ExposureTier, [u8; 4])] = &[
        // No declared endpoint config (legacy/static mode) → loopback always.
        (None, ExposureTier::Internet, [127, 0, 0, 1]),
        // Declared Local → loopback regardless of ceiling.
        (
            Some(ExposureTier::Local),
            ExposureTier::Internet,
            [127, 0, 0, 1],
        ),
        // Declared wide and honored by the ceiling → wide bind.
        (Some(ExposureTier::Lan), ExposureTier::Lan, [0, 0, 0, 0]),
        (
            Some(ExposureTier::Mesh),
            ExposureTier::Internet,
            [0, 0, 0, 0],
        ),
        // Ceiling narrowed below the declared tier → loopback (fail-safe).
        (
            Some(ExposureTier::Mesh),
            ExposureTier::Local,
            [127, 0, 0, 1],
        ),
        (
            Some(ExposureTier::Internet),
            ExposureTier::Mesh,
            [127, 0, 0, 1],
        ),
    ];
    for (declared, ceiling, expected) in cases {
        let h = harness(false, *ceiling, HashMap::new());
        *h.state.declared_exposure.write().unwrap() = *declared;
        let manager = ListenerManager::new(9100, h.state.clone());
        assert_eq!(
            manager.effective_addr(),
            SocketAddr::from((*expected, 9100)),
            "declared={declared:?} ceiling={ceiling:?}"
        );
    }
}

// ── deliver(): streaming accumulation and error outcomes ──────────────────────

#[tokio::test]
async fn deliver_accumulates_streaming_tokens_into_empty_final_reply() {
    let h = harness(false, ExposureTier::Local, HashMap::new());
    let mut guest = McpMembrane::new(9100, "mcp-membrane-test", h.state.clone());

    let (tx, rx) = oneshot::channel();
    h.state
        .pending_responses
        .lock()
        .await
        .insert("turn-1".into(), tx);

    for token in ["hel", "lo"] {
        guest
            .deliver(OutboundReply::StreamingToken {
                session_id: "s".into(),
                turn_id: "turn-1".into(),
                token: token.into(),
            })
            .await
            .unwrap();
    }
    guest
        .deliver(OutboundReply::Text {
            session_id: "s".into(),
            turn_id: "turn-1".into(),
            content: "".into(),
            reply_to: None,
        })
        .await
        .unwrap();

    match rx.await.unwrap() {
        DispatchOutcome::Ok(content) => assert_eq!(content, "hello"),
        other => panic!("expected Ok, got {other:?}"),
    }
    assert!(h.state.streaming_buffers.lock().await.is_empty());
}

#[tokio::test]
async fn deliver_drops_orphan_streams_and_bounds_live_buffer() {
    let h = harness(false, ExposureTier::Local, HashMap::new());
    let mut guest = McpMembrane::new(9100, "mcp-membrane-test", h.state.clone());
    guest
        .deliver(OutboundReply::StreamingToken {
            session_id: "synthetic".into(),
            turn_id: "missing".into(),
            token: "synthetic".into(),
        })
        .await
        .unwrap();
    assert!(h.state.streaming_buffers.lock().await.is_empty());
    let (tx, rx) = oneshot::channel();
    h.state
        .pending_responses
        .lock()
        .await
        .insert("bounded".into(), tx);
    guest
        .deliver(OutboundReply::StreamingToken {
            session_id: "synthetic".into(),
            turn_id: "bounded".into(),
            token: "x".repeat(1024 * 1024 + 1),
        })
        .await
        .unwrap();
    assert!(matches!(rx.await.unwrap(), DispatchOutcome::Err(_)));
    assert!(h.state.pending_responses.lock().await.is_empty());
    assert!(h.state.streaming_buffers.lock().await.is_empty());
}

#[tokio::test]
async fn deliver_error_reply_yields_err_outcome() {
    let h = harness(false, ExposureTier::Local, HashMap::new());
    let mut guest = McpMembrane::new(9100, "mcp-membrane-test", h.state.clone());

    let (tx, rx) = oneshot::channel();
    h.state
        .pending_responses
        .lock()
        .await
        .insert("turn-2".into(), tx);

    guest
        .deliver(OutboundReply::Error {
            session_id: "s".into(),
            turn_id: "turn-2".into(),
            message: "philote failed".into(),
        })
        .await
        .unwrap();

    match rx.await.unwrap() {
        DispatchOutcome::Err(message) => assert_eq!(message, "philote failed"),
        other => panic!("expected Err, got {other:?}"),
    }
}

// ── Loopback trust behind a reverse proxy ─────────────────────────────────────

#[test]
fn proxied_loopback_request_is_not_trusted_as_local() {
    use crate::server::is_trusted_loopback;
    use axum::http::{HeaderMap, HeaderValue};

    let loopback: SocketAddr = "127.0.0.1:50000".parse().unwrap();
    let remote: SocketAddr = "100.64.0.7:50000".parse().unwrap();

    assert!(is_trusted_loopback(&loopback, &HeaderMap::new()));
    assert!(!is_trusted_loopback(&remote, &HeaderMap::new()));

    // The TLS proxy on vps-jane reaches the listener over 127.0.0.1 and adds
    // forwarding headers; any of them marks the request as remote.
    for header in ["x-forwarded-for", "forwarded", "x-real-ip"] {
        let mut headers = HeaderMap::new();
        headers.insert(header, HeaderValue::from_static("203.0.113.9"));
        assert!(
            !is_trusted_loopback(&loopback, &headers),
            "{header} on a loopback peer must not be trusted as local"
        );
    }
}

// ── Upstream CallToolResult is not double-wrapped ─────────────────────────────

#[test]
fn upstream_call_tool_result_passes_through_unwrapped() {
    use crate::server::mcp_call_result_passthrough;

    let upstream = json!({ "content": [{ "type": "text", "text": "{\"count\":1}" }] });
    assert_eq!(
        mcp_call_result_passthrough(&upstream),
        Some(upstream.clone())
    );

    let upstream_error =
        json!({ "content": [{ "type": "text", "text": "nope" }], "isError": true });
    assert_eq!(
        mcp_call_result_passthrough(&upstream_error),
        Some(upstream_error.clone())
    );

    // Plain datasource payloads keep the JSON-as-text wrapping.
    assert_eq!(mcp_call_result_passthrough(&json!({ "count": 1 })), None);
    assert_eq!(
        mcp_call_result_passthrough(&json!({ "content": [{ "text": "untyped" }] })),
        None
    );
    assert_eq!(
        mcp_call_result_passthrough(&json!({ "content": [], "rows": 3 })),
        None
    );
}
