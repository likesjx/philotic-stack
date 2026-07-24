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
