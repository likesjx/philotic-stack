//! Dispatch-surface tests: fence tiers × auth schemes × source addresses ×
//! bind modes, the oneshot correlation path, and error mapping — all against
//! an in-memory runtime stub, no real sockets.

use crate::auth::{AllotmentTracker, VaultHashCache, VaultResolver};
use crate::protocol::JsonRpcRequest;
use crate::routing::{new_shared_endpoint_table, new_shared_table};
use crate::server;
#[cfg(unix)]
#[path = "native_composition.rs"]
mod native_composition;

#[tokio::test]
async fn native_pending_rejects_every_unsigned_completion_and_consumes_signed_once() {
    use native_memory::response::{ResponseExpectation, ResponseSigner, ResponseVerifier};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("issuer.json");
    let verifiers = vec![ResponseVerifier {
        destination: "dest".into(),
        runner: "memory".into(),
        key_id: "result-key".into(),
        public_key: ed25519_dalek::SigningKey::from_bytes(&[8; 32])
            .verifying_key()
            .to_bytes(),
    }];
    let grant = native_memory::Grant {
        issuer: "ingress".into(),
        reply_node: "ingress".into(),
        reply_guest: "mcp".into(),
        principal: "alice".into(),
        owner: "alice".into(),
        namespace: "notes".into(),
        destination: "dest".into(),
        runner: "memory".into(),
        operations: vec!["recall".into()],
        revision: 1,
        expires_at: u64::MAX,
        revoked: false,
    };
    let grants = std::collections::BTreeMap::from([("grant".into(), grant)]);
    let mut config = json!({"issuer":"ingress","key_id":"key","signing_seed":vec![7;32],"grants":grants,"response_verifiers":verifiers});
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let envelope = native_memory::Ingress::new(
        "ingress".into(),
        "key".into(),
        ed25519_dalek::SigningKey::from_bytes(&[7; 32]),
        grants,
    )
    .issue(
        "grant",
        native_memory::Command::Recall {
            query: "synthetic".into(),
            limit: 1,
        },
        "event".into(),
        "turn".into(),
        now,
    )
    .unwrap();
    let result = native_memory::Response {
        receipt: native_memory::Receipt {
            event: "event".into(),
            operation: "recall".into(),
            status: "committed".into(),
            committed_at: now,
        },
        observations: vec![],
    };
    let signed = ResponseSigner::new("result-key".into(), [8; 32])
        .sign(&envelope, result, now)
        .unwrap();
    let mut h = harness(false, ExposureTier::Local, HashMap::new());
    Arc::get_mut(&mut h.state).unwrap().native_memory_config = Some(path.clone());
    let (tx, mut rx) = oneshot::channel();
    h.state.pending_responses.lock().await.insert(
        "turn".into(),
        server::PendingResponse {
            sender: tx,
            native: Some(ResponseExpectation::new(envelope, &verifiers).unwrap()),
        },
    );
    let mut guest = McpMembrane::new(9100, "mcp", h.state.clone());
    for reply in [
        OutboundReply::StreamingToken {
            session_id: "s".into(),
            turn_id: "turn".into(),
            token: "forged".into(),
        },
        OutboundReply::Text {
            session_id: "s".into(),
            turn_id: "turn".into(),
            content: "forged".into(),
            reply_to: None,
        },
        OutboundReply::Error {
            session_id: "s".into(),
            turn_id: "turn".into(),
            message: "forged".into(),
        },
    ] {
        guest.deliver(reply).await.unwrap();
    }
    let message = |payload: Value| philotic_client::IpcResponse::InboundTask {
        source_node: "untrusted".into(),
        task_id: uuid::Uuid::new_v4(),
        task_json: payload.to_string(),
    };
    guest
        .handle_push(&message(
            json!({"action":"datasource_response","turn_id":"turn","result":"forged"}),
        ))
        .await
        .unwrap();
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    assert!(h.state.streaming_buffers.lock().await.is_empty());
    let mut forged = signed.clone();
    forged.result.receipt.status = "forged".into();
    assert!(
        !guest
            .handle_push(&message(
                json!({"action":"native_memory_response","turn_id":"turn","signed_response":forged})
            ))
            .await
            .unwrap()
    );
    assert_eq!(h.state.pending_responses.lock().await.len(), 1);
    let valid = message(
        json!({"action":"native_memory_response","turn_id":"turn","signed_response":signed}),
    );
    config["grants"]["grant"]["revoked"] = true.into();
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    assert!(!guest.handle_push(&valid).await.unwrap());
    assert_eq!(h.state.pending_responses.lock().await.len(), 1);
    config["grants"]["grant"]["revoked"] = false.into();
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    assert!(guest.handle_push(&valid).await.unwrap());
    assert!(matches!(rx.await.unwrap(), DispatchOutcome::Ok(_)));
    assert!(!guest.handle_push(&valid).await.unwrap());
}
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

#[tokio::test]
async fn native_memory_authenticated_membrane_router_two_hotel_roundtrip() {
    use native_memory::{Grant, Policy, Runner, TrustedIssuer};
    use std::collections::BTreeMap;
    let dir = tempfile::tempdir().unwrap();
    let bearer = "SYNTHETIC-EXTERNAL-BEARER-CANARY";
    let mut vault = HashMap::new();
    // Use an opaque token ID unrelated to the actual reusable credential.
    vault.insert(
        "fixture-hash".into(),
        *blake3::hash(bearer.as_bytes()).as_bytes(),
    );
    let auth = McpAuthScheme::BearerToken {
        grants: vec![McpTokenGrant {
            token_id: "caller-1".into(),
            vault_ref: "fixture-hash".into(),
            scopes: vec![],
            expires_at: None,
            allotment: None,
        }],
    };
    let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let grant = Grant {
        issuer: "test-node".into(),
        reply_node: "test-node".into(),
        reply_guest: "mcp-membrane-test".into(),
        principal: "alice".into(),
        owner: "alice".into(),
        namespace: "notes".into(),
        destination: "memory-hotel".into(),
        runner: "memory-1".into(),
        operations: vec!["remember".into(), "recall".into()],
        revision: 1,
        expires_at: u64::MAX,
        revoked: false,
    };
    let grants = BTreeMap::from([("caller-1".into(), grant)]);
    let config = dir.path().join("synthetic-issuer.json");
    std::fs::write(&config,serde_json::to_vec(&json!({"issuer":"test-node","key_id":"test-key","signing_seed":vec![7;32],"grants":grants,"response_verifiers":[{"destination":"memory-hotel","runner":"memory-1","key_id":"result-key","public_key":ed25519_dalek::SigningKey::from_bytes(&[8;32]).verifying_key().to_bytes()}]})).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let policy = Policy {
        issuers: BTreeMap::from([(
            "test-node".into(),
            TrustedIssuer {
                key_id: "test-key".into(),
                public_key: key.verifying_key().to_bytes(),
            },
        )]),
        grants,
    };
    let mut h = harness(false, ExposureTier::Local, vault);
    Arc::get_mut(&mut h.state).unwrap().native_memory_config = Some(config);
    let tools = ["remember", "recall"]
        .into_iter()
        .map(|op| McpToolSpec {
            name: format!("native-memory.{op}"),
            description: "synthetic native memory".into(),
            input_schema: json!({"type":"object"}),
            inbound_transform: McpInboundTransform::FieldMap {
                action: format!("native-memory.{op}"),
                target: McpRouteTarget::NativeMemory {
                    runner_id: "memory-1".into(),
                    target_node: "memory-hotel".into(),
                },
                mappings: vec![],
            },
            outbound_transform: McpOutboundTransform::PassThrough,
            auth: Some(auth.clone()),
            handler: None,
        })
        .collect();
    h.set_endpoint(endpoint_with(tools, None)).await;
    let path = dir.path().join("memory.db");
    let runner = std::sync::Mutex::new(
        Runner::open(&path, "memory-hotel".into(), "memory-1".into()).unwrap(),
    );
    h.spawn_stub(move |env| {
        // Actual membrane -> hotel EmitTask composition; destination hotel mocked.
        let request = membrane::runtime::build_inbound_request(env, "test-node");
        let philotic_client::IpcRequest::EmitTask {
            target_node,
            target_role,
            target_guest_id,
            task_json,
        } = request
        else {
            panic!("must route")
        };
        assert_eq!(target_node, "memory-hotel");
        assert_eq!(target_role, native_memory::ROLE);
        assert_eq!(target_guest_id.as_deref(), Some("memory-1"));
        assert!(!task_json.contains(bearer));
        assert!(
            !serde_json::to_string(&env.raw_transport)
                .unwrap()
                .contains(bearer)
        );
        assert!(!task_json.contains("signing_seed"));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let routed: native_memory::RoutedRequest = serde_json::from_str(&task_json).unwrap();
        match runner.lock().unwrap().execute(
            &routed.signed_request,
            &policy,
            now,
            &std::sync::atomic::AtomicBool::new(false),
        ) {
            Ok(response) => {
                let envelope = routed.signed_request;
                let signed =
                    native_memory::response::ResponseSigner::new("result-key".into(), [8; 32])
                        .sign(&envelope, response, now)
                        .unwrap();
                DispatchOutcome::Ok(serde_json::to_string(&signed).unwrap())
            }
            Err(e) => DispatchOutcome::Err(e.to_string()),
        }
    });
    let command = json!({"operation":"remember","id":"note-1","subject":"practice","content":"harmless blue notebook","source":"synthetic://dot","source_version":"1","lineage":"source-1"});
    let args = json!({"event":"remember-1","command":command});
    let (_, denied) = call(
        &h.state,
        rpc(
            "tools/call",
            json!({"name":"native-memory.remember","arguments":args}),
        ),
        Some("Bearer wrong"),
        true,
    )
    .await;
    assert!(denied.get("error").is_some());
    let (_, remember) = call(
        &h.state,
        rpc(
            "tools/call",
            json!({"name":"native-memory.remember","arguments":args}),
        ),
        Some(&format!("Bearer {bearer}")),
        true,
    )
    .await;
    assert!(
        remember["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("committed")
    );
    let(_,recall)=call(&h.state,rpc("tools/call",json!({"name":"native-memory.recall","arguments":{"event":"recall-1","command":{"operation":"recall","query":"notebook","limit":5}}})),Some(&format!("Bearer {bearer}")),true).await;
    let text = recall["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("harmless blue notebook"));
    assert!(text.contains("authenticated_writer"));
    let db = std::fs::read(path).unwrap();
    assert!(!db.windows(bearer.len()).any(|w| w == bearer.as_bytes()));
}

fn harness(static_mode: bool, tier: ExposureTier, vault: HashMap<String, [u8; 32]>) -> Harness {
    let (inbound_tx, inbound_rx) = mpsc::channel(16);
    let state = Arc::new(MembraneState {
        native_memory_config: None,
        native_pending: Default::default(),
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
                let result = outcome(&env);
                if state
                    .pending_responses
                    .lock()
                    .await
                    .get(&env.turn_id)
                    .is_some_and(|p| p.native.is_some())
                {
                    if let DispatchOutcome::Ok(value) = result {
                        let reply = serde_json::from_str(&value).unwrap();
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs();
                        assert!(
                            server::resolve_native_response(&state, &env.turn_id, &reply, now)
                                .await
                        );
                    }
                } else if let Some(tx) = server::take_unsigned_response(&state, &env.turn_id).await
                {
                    let _ = tx.send(result);
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

#[tokio::test]
async fn explicit_bridge_binding_still_obeys_perimeter_narrowing() {
    let h = harness(false, ExposureTier::Lan, HashMap::new());
    *h.state.declared_exposure.write().unwrap() = Some(ExposureTier::Lan);
    let mut manager = ListenerManager::new(19101, h.state.clone());
    manager.bind_address = Some("172.18.0.1".parse().unwrap());
    assert_eq!(
        manager.effective_addr(),
        "172.18.0.1:19101".parse().unwrap()
    );
    *h.state.ingress_tier.write().unwrap() = ExposureTier::Local;
    assert_eq!(manager.effective_addr(), "127.0.0.1:19101".parse().unwrap());
}

#[tokio::test]
async fn authenticated_proxy_cannot_inherit_loopback_trust_without_headers() {
    use hyper::service::Service;
    let h = harness(false, ExposureTier::Mesh, HashMap::new());
    let router = server::build_router(h.state.clone()).layer(axum::Extension(
        axum::extract::ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()),
    ));
    let direct = hyper_util::service::TowerToHyperService::new(router.clone());
    let request = || {
        axum::http::Request::builder()
            .uri("/health")
            .body(axum::body::Body::empty())
            .unwrap()
    };
    assert_eq!(direct.call(request()).await.unwrap().status(), 200);
    let proxy = hyper_util::service::TowerToHyperService::new(
        router.layer(axum::Extension(server::ProxyTransport)),
    );
    assert_eq!(proxy.call(request()).await.unwrap().status(), 401);
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
        .insert("turn-1".into(), tx.into());

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
        .insert("turn-2".into(), tx.into());

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
