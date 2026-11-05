//! Real UDS hotel tests. Runner payload/results are synthetic here; the
//! process composition test separately exercises the real LifeGraph provider.
use super::*;
use ansible_mesh_core::storage::{GuestRecord, HotelRecord};
use ansible_mesh_core::{NodeCapabilities, sqlite_storage::SqliteGraphStorage};
use philotic_client::PhiloticClient;

struct Fixture {
    graph: Arc<GraphDomain>,
    socket: String,
    server: tokio::task::JoinHandle<()>,
    inboxes: InboxRegistry,
    leases: Arc<Mutex<RuntimeLeaseRegistry>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_file(&self.socket);
    }
}
impl Fixture {
    async fn new() -> Self {
        let socket = format!("/tmp/runner-tools-{}.sock", Uuid::new_v4());
        let store = SqliteGraphStorage::open(":memory:").unwrap();
        let graph = Arc::new(GraphDomain::new(Arc::new(store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "fixture".into(),
                capabilities: NodeCapabilities {
                    node_id: "fixture-node".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                    build_version: String::new(),
                },
                mesh_port: 0,
                blob_port: 0,
                execution_port: 0,
                ipc_socket_path: socket.clone(),
                active_pid: None,
                mesh_host: None,
            })
            .unwrap();
        graph
            .upsert_guest(&GuestRecord {
                hotel_name: "fixture".into(),
                guest_id: "life-fixture".into(),
                role: "life-graph-runner".into(),
                config_json: "{}".into(),
                is_active: true,
                active_pid: None,
                last_active_at: None,
            })
            .unwrap();
        let now = unix_ts();
        graph.set_config_value("__mcp_endpoint__:fixture", &serde_json::json!({
            "endpoint_id":"fixture","owner_agent_id":"fixture-owner","port":19000,"updated_at":now,
            "tools":[{"name":"node-recall","description":"must be replaced","input_schema":{"type":"object"},
                "inbound_transform":{"kind":"field_map","action":"life.view.node","target":{"kind":"runner_tool","runner_id":"life-fixture","tool_name":"life.view.node","version":1},"mappings":[]},
                "outbound_transform":{"kind":"pass_through"},"auth":{"scheme":"bearer_token","grants":[{"token_id":"fixture-caller","vault_ref":"synthetic","scopes":["node-recall"],"expires_at":now+60}]}}],
            "preapproval_rules":[{"action_pattern":"life.view.node","target":{"kind":"runner_tool","runner_id":"life-fixture","tool_name":"life.view.node","version":1},"approved_by_turn":"synthetic-fixture","approved_at":now,"expires_at":now+60}]
        }).to_string()).unwrap();
        let (tx, _rx) = super::super::test_dispatcher_channel();
        let server = IpcServer::new(socket.clone(), "fixture-node", tx, graph.clone());
        let inboxes = server.inboxes();
        let leases = server.mcp_membrane_leases.clone();
        let server = tokio::spawn(async move {
            server.run().await.unwrap();
        });
        for _ in 0..100 {
            if std::path::Path::new(&socket).exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        Self {
            graph,
            socket,
            server,
            inboxes,
            leases,
        }
    }
    async fn connect(&self, guest: &str, role: &str, tools: Vec<String>) -> PhiloticClient {
        PhiloticClient::connect_at(
            &self.socket,
            GuestIdentity {
                guest_id: guest.into(),
                role: role.into(),
                supported_tools: tools,
            },
        )
        .await
        .unwrap()
    }
    async fn runner(&self) -> PhiloticClient {
        self.connect(
            "life-fixture",
            "life-graph-runner",
            vec!["life.view.node".into()],
        )
        .await
    }
    async fn membrane(&self) -> PhiloticClient {
        let mut client = self
            .connect("mcp-membrane-fixture", "mcp-membrane", vec![])
            .await;
        assert!(matches!(
            client
                .send_request(IpcRequest::AcquireMcpMembraneLease {
                    lease_key: "mcp-membrane-fixture".into(),
                    port: 19000
                })
                .await
                .unwrap(),
            IpcResponse::McpMembraneLease {
                mcp_granted: true,
                ..
            }
        ));
        client
    }
}
fn advertisement() -> RunnerToolAdvertisement {
    RunnerToolAdvertisement {
        protocol_version: 1,
        revision: 1,
        tools: vec![RunnerToolDescriptor {
            name: "life.view.node".into(),
            version: 1,
            description: "synthetic node read".into(),
            read_only: true,
            input_schema: serde_json::json!({"type":"object","additionalProperties":false,"required":["id"],"properties":{"id":{"type":"string","minLength":1,"maxLength":512},"edge_limit":{"type":"integer","minimum":1,"maximum":200}}}),
        }],
    }
}
async fn advertise(client: &mut PhiloticClient, ad: RunnerToolAdvertisement) -> bool {
    matches!(
        client
            .send_request(IpcRequest::AdvertiseRunnerTools { advertisement: ad })
            .await
            .unwrap(),
        IpcResponse::Standard { ok: true, .. }
    )
}
async fn visible(client: &mut PhiloticClient, endpoint: &str, tool: &str) -> bool {
    matches!(
        client
            .send_request(IpcRequest::GetRunnerTool {
                endpoint_id: endpoint.into(),
                tool_name: tool.into(),
                caller_id: "fixture-caller".into(),
            })
            .await
            .unwrap(),
        IpcResponse::Standard { ok: true, .. }
    )
}
fn invocation(caller: &str, arguments: serde_json::Value) -> IpcRequest {
    IpcRequest::InvokeRunnerTool {
        endpoint_id: "fixture".into(),
        tool_name: "node-recall".into(),
        version: 1,
        arguments,
        caller_id: caller.into(),
        session_id: "session-1".into(),
        turn_id: "turn-1".into(),
    }
}

#[tokio::test]
async fn native_runner_tools_real_uds_registration_scope_replacement_and_return() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.runner().await;
    assert!(advertise(&mut runner, advertisement()).await);
    assert!(
        advertise(&mut runner, advertisement()).await,
        "idempotent retry"
    );
    let mut newer = advertisement();
    newer.revision = 2;
    assert!(advertise(&mut runner, newer).await);
    assert!(
        !advertise(&mut runner, advertisement()).await,
        "older revision cannot replace newer publication"
    );
    let mut conflicting = advertisement();
    conflicting.tools[0].description = "changed".into();
    assert!(!advertise(&mut runner, conflicting.clone()).await);
    conflicting.revision = 3;
    assert!(
        !advertise(&mut runner, conflicting).await,
        "same version semantics are immutable"
    );
    let mut membrane = fixture.membrane().await;
    assert!(visible(&mut membrane, "fixture", "node-recall").await);
    for caller in ["", "wrong-caller"] {
        assert!(matches!(
            membrane
                .send_request(IpcRequest::GetRunnerTool {
                    endpoint_id: "fixture".into(),
                    tool_name: "node-recall".into(),
                    caller_id: caller.into()
                })
                .await
                .unwrap(),
            IpcResponse::Standard { ok: false, .. }
        ));
    }
    let mut old_wire = serde_json::to_value(IpcRequest::GetRunnerTool {
        endpoint_id: "fixture".into(),
        tool_name: "node-recall".into(),
        caller_id: "fixture-caller".into(),
    })
    .unwrap();
    old_wire["payload"]
        .as_object_mut()
        .unwrap()
        .remove("caller_id");
    let old_request = serde_json::from_value(old_wire).unwrap();
    assert!(
        matches!(
            membrane.send_request(old_request).await.unwrap(),
            IpcResponse::Standard { ok: false, .. }
        ),
        "unbound prototype descriptor request must fail closed"
    );
    assert!(!visible(&mut membrane, "another-endpoint", "node-recall").await);
    assert!(!visible(&mut membrane, "fixture", "life.observe").await);
    let mut intruder = fixture.connect("intruder", "agent", vec![]).await;
    assert!(!visible(&mut intruder, "fixture", "node-recall").await);
    for request in [
        invocation("wrong-caller", serde_json::json!({"id":"goal"})),
        invocation(
            "fixture-caller",
            serde_json::json!({"id":"goal","reply_guest_id":"intruder"}),
        ),
        invocation(
            "fixture-caller",
            serde_json::json!({"id":"goal","edge_limit":201}),
        ),
    ] {
        assert!(matches!(
            membrane.send_request(request).await.unwrap(),
            IpcResponse::Standard { ok: false, .. }
        ));
    }
    assert!(matches!(
        membrane
            .send_request(invocation(
                "fixture-caller",
                serde_json::json!({"id":"goal","edge_limit":1})
            ))
            .await
            .unwrap(),
        IpcResponse::Standard { ok: true, .. }
    ));
    let IpcResponse::InboundTask { task_json, .. } = runner.recv_task().await.unwrap() else {
        panic!("runner task")
    };
    let task: serde_json::Value = serde_json::from_str(&task_json).unwrap();
    assert_eq!(task["identity"]["agent_id"], "mcp:fixture-caller");
    assert_eq!(task["reply_guest_id"], "mcp-membrane-fixture");
    assert_eq!(task["turn_id"], "turn-1");
    assert_eq!(task["tool_name"], "life.view.node");
    runner.send_request(IpcRequest::EmitTask {target_node:"fixture-node".into(),target_role:"mcp-membrane".into(),target_guest_id:Some("mcp-membrane-fixture".into()),task_json:serde_json::json!({"action":"datasource_response","turn_id":"turn-1","result":{"synthetic":true}}).to_string()}).await.unwrap();
    assert!(
        matches!(membrane.recv_task().await.unwrap(),IpcResponse::InboundTask {task_json,..} if task_json.contains("turn-1"))
    );
    let mut replacement = fixture.runner().await;
    assert!(
        !visible(&mut membrane, "fixture", "node-recall").await,
        "new connection needs fresh publication"
    );
    assert!(
        !advertise(&mut runner, advertisement()).await,
        "superseded connection cannot advertise"
    );
    assert!(advertise(&mut replacement, advertisement()).await);
    assert!(visible(&mut membrane, "fixture", "node-recall").await);
    let mut conflicting = advertisement();
    conflicting.tools[0].description = "changed after restart".into();
    let mut restarted = fixture.runner().await;
    assert!(
        !advertise(&mut restarted, conflicting).await,
        "persisted contract survives connection restart"
    );
    assert!(advertise(&mut restarted, advertisement()).await);
    membrane
        .send_request(IpcRequest::ReleaseMcpMembraneLease {
            lease_key: "mcp-membrane-fixture".into(),
        })
        .await
        .unwrap();
    assert!(
        !visible(&mut membrane, "fixture", "node-recall").await,
        "lease loss removes projection"
    );
    assert!(matches!(
        membrane
            .send_request(invocation(
                "fixture-caller",
                serde_json::json!({"id":"goal"})
            ))
            .await
            .unwrap(),
        IpcResponse::Standard { ok: false, .. }
    ));
}

#[tokio::test]
async fn native_runner_tools_do_not_grant_from_publication_or_disabled_enrollment() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.runner().await;
    let mut bad = advertisement();
    bad.tools[0].name = "graph.query".into();
    assert!(!advertise(&mut runner, bad).await);
    let mut bad = advertisement();
    bad.tools[0].read_only = false;
    assert!(!advertise(&mut runner, bad).await);
    let mut bad = advertisement();
    bad.protocol_version = 2;
    assert!(!advertise(&mut runner, bad).await);
    let mut bad = advertisement();
    bad.tools.push(bad.tools[0].clone());
    assert!(!advertise(&mut runner, bad).await);
    let mut bad = advertisement();
    bad.tools[0].input_schema["properties"]["id"]["pattern"] = "^allowed$".into();
    assert!(
        !advertise(&mut runner, bad).await,
        "unsupported constraints cannot be silently ignored"
    );
    assert!(advertise(&mut runner, advertisement()).await);
    let mut membrane = fixture.membrane().await;
    assert!(visible(&mut membrane, "fixture", "node-recall").await);
    let mut versioned = advertisement();
    versioned.revision = 2;
    versioned.tools[0].version = 2;
    assert!(advertise(&mut runner, versioned).await);
    assert!(
        !visible(&mut membrane, "fixture", "node-recall").await,
        "endpoint must explicitly approve the new tool version"
    );
    assert!(matches!(
        membrane
            .send_request(invocation(
                "fixture-caller",
                serde_json::json!({"id":"goal"})
            ))
            .await
            .unwrap(),
        IpcResponse::Standard { ok: false, .. }
    ));
    let mut guest = fixture
        .graph
        .get_guest("fixture", "life-fixture")
        .unwrap()
        .unwrap();
    guest.is_active = false;
    fixture.graph.upsert_guest(&guest).unwrap();
    assert!(!visible(&mut membrane, "fixture", "node-recall").await);
    assert!(!advertise(&mut runner, advertisement()).await);
}

#[tokio::test]
async fn native_runner_tools_assembly_uses_only_preselected_authorized_routes() {
    let fixture = Fixture::new().await;
    let mut runner = fixture.runner().await;
    assert!(advertise(&mut runner, advertisement()).await);
    let mut empty = serde_json::json!({"tools_for_model":[],"execution_routes":{}});
    IpcServer::project_runner_definitions(&fixture.inboxes, &mut empty, "fixture-node").await;
    assert_eq!(
        empty["tools_for_model"],
        serde_json::json!([]),
        "publication cannot create a grant"
    );
    let mut assembly = serde_json::json!({"tools_for_model":[{"tool_name":"life.view.node","description":"legacy","input_schema":{"type":"object"}}],"execution_routes":{"life.view.node":{"runner_id":"life-fixture","target_node":"other-node"}}});
    IpcServer::project_runner_definitions(&fixture.inboxes, &mut assembly, "fixture-node").await;
    assert_eq!(
        assembly["tools_for_model"][0]["description"], "legacy",
        "local publication cannot replace a remote owner's contract"
    );
    assembly["execution_routes"]["life.view.node"]["target_node"] = "fixture-node".into();
    IpcServer::project_runner_definitions(&fixture.inboxes, &mut assembly, "fixture-node").await;
    assert_eq!(
        assembly["tools_for_model"][0]["input_schema"]["required"],
        serde_json::json!(["id"])
    );
    assert_eq!(
        assembly["execution_routes"]["life.view.node"]["descriptor_version"],
        1
    );
}

#[tokio::test]
async fn native_runner_tools_expired_approval_and_expired_lease_fail_closed() {
    use crate::service::lease::NoopLeaseObserver;
    let fixture = Fixture::new().await;
    let mut runner = fixture.runner().await;
    assert!(advertise(&mut runner, advertisement()).await);
    let mut membrane = fixture.membrane().await;
    assert!(visible(&mut membrane, "fixture", "node-recall").await);
    let original = fixture
        .graph
        .get_config_value("__mcp_endpoint__:fixture")
        .unwrap()
        .unwrap();
    let mut config: serde_json::Value = serde_json::from_str(&original).unwrap();
    config["preapproval_rules"][0]["expires_at"] = 0.into();
    fixture
        .graph
        .set_config_value("__mcp_endpoint__:fixture", &config.to_string())
        .unwrap();
    assert!(!visible(&mut membrane, "fixture", "node-recall").await);
    fixture
        .graph
        .set_config_value("__mcp_endpoint__:fixture", &original)
        .unwrap();
    let connection = fixture.inboxes.lock().await["mcp-membrane"][0].conn_id;
    {
        let mut leases = fixture.leases.lock().await;
        let mut observer = NoopLeaseObserver;
        let candidate = leases
            .release("mcp-membrane-fixture", connection, &mut observer)
            .unwrap();
        leases.acquire(connection, candidate, 0, unix_ts(), &mut observer);
    }
    assert!(
        !visible(&mut membrane, "fixture", "node-recall").await,
        "active status alone cannot extend an expired deadline"
    );
    assert!(matches!(
        membrane
            .send_request(invocation(
                "fixture-caller",
                serde_json::json!({"id":"goal"})
            ))
            .await
            .unwrap(),
        IpcResponse::Standard { ok: false, .. }
    ));
}

#[test]
fn native_runner_tools_observation_has_only_allowlisted_metadata() {
    let canary = "SYNTHETIC-SECRET-IDENTITY-CANARY";
    let observation = NativeToolObservation {
        phase: "invoke",
        allowed: false,
        endpoint: canary,
        exposed_tool: canary,
        caller: canary,
        session: canary,
        turn: canary,
        operation: None,
        elapsed: std::time::Duration::from_micros(17),
    };
    let metadata = observation.metadata();
    let keys = metadata
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        keys,
        std::collections::BTreeSet::from([
            "event",
            "phase",
            "policy_outcome",
            "policy_code",
            "endpoint_ref",
            "tool_ref",
            "caller_ref",
            "session_ref",
            "turn_ref",
            "operation",
            "admission_duration_us"
        ])
    );
    assert!(!metadata.to_string().contains(canary));
    assert_eq!(metadata["policy_outcome"], "denied");
    assert_eq!(metadata["admission_duration_us"], 17);
}
