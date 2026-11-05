//! Opt-in process composition: real hotel UDS routing, native runner, and MCP runtime.
//! No installed configuration, environment credentials, or provider guests are used.
use super::*;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const SOURCE: &str = "fixture-ingress-aiua-01";
const DESTINATION: &str = "fixture-memory-aiua-01";
const GUEST: &str = "mcp-membrane-fixture";
const RUNNER: &str = "memory-fixture";
const BEARER: &str = "SYNTHETIC-COMPOSITION-BEARER";

struct Process(Child);
struct Diagnostics(PathBuf);
impl Drop for Diagnostics {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        // Only explicit logs created by this synthetic fixture; never config/key files.
        let mut paths = vec![self.0.join("runner.log")];
        for hotel in ["source", "destination", "hotel"] {
            paths.push(self.0.join(hotel).join("process.log"));
            if let Ok(entries) = std::fs::read_dir(self.0.join(hotel).join("logs")) {
                paths.extend(entries.flatten().map(|entry| entry.path()));
            }
        }
        for path in paths {
            if let Ok(log) = std::fs::read_to_string(&path) {
                eprintln!("Synthetic fixture log {}:\n{}", path.display(), log);
            }
        }
    }
}
impl Process {
    fn stop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
    }
}
struct Task(tokio::task::JoinHandle<anyhow::Result<()>>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn private_json(path: &Path, value: &impl serde::Serialize) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(&serde_json::to_vec(value).unwrap()).unwrap();
}

fn hotel(bin: &Path, root: &Path, name: &str, socket: &Path) -> Process {
    std::fs::create_dir_all(root).unwrap();
    let log = std::fs::File::create(root.join("process.log")).unwrap();
    Process(
        Command::new(bin.join("aiua"))
            .env_clear()
            .current_dir(root)
            .env("CI", "true")
            .env("PHILOTIC_SMOKE_MODE", "1")
            .env("PHILOTIC_HOTEL_SOCKET", socket)
            .env("PHILOTIC_LOG_DIR", root.join("logs"))
            .args(["--hotel", name])
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("start disposable aiua"),
    )
}

async fn connect(socket: &Path, guest: &str, role: &str) -> PhiloticClient {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(client) = PhiloticClient::connect_at(
            socket.to_str().unwrap(),
            GuestIdentity {
                guest_id: guest.into(),
                role: role.into(),
                supported_tools: vec![],
            },
        )
        .await
        {
            return client;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "hotel IPC readiness timed out"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn seed(
    client: &mut PhiloticClient,
    node: &str,
    hotel: &str,
    guest: &str,
    role: &str,
    socket: &Path,
) {
    let result = client
        .send_request(IpcRequest::SeedRemoteIncarnation {
            node_id: node.into(),
            hotel_id: hotel.into(),
            incarnation_id: format!("{node}:{guest}"),
            target_role: role.into(),
            socket_path: Some(socket.to_str().unwrap().into()),
        })
        .await
        .unwrap();
    assert!(matches!(result, IpcResponse::Standard { ok: true, .. }));
}

async fn http(port: u16, operation: &str, event: &str, command: Value) -> Value {
    http_rpc(port, "tools/call", json!({
        "name":format!("native-memory.{operation}"),"arguments":{"event":event,"command":command}
    })).await
}

async fn http_rpc(port: u16, method: &str, params: Value) -> Value {
    http_rpc_authenticated(port, method, params, BEARER).await
}

async fn http_rpc_authenticated(port: u16, method: &str, params: Value, bearer: &str) -> Value {
    let body = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string();
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {bearer}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let split = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("HTTP headers");
    serde_json::from_slice(&response[split + 4..]).expect("HTTP JSON response")
}

fn result(response: &Value) -> Value {
    assert!(response.get("error").is_none(), "{response}");
    assert_ne!(response["result"]["isError"], true, "{response}");
    serde_json::from_str(
        response["result"]["content"][0]["text"]
            .as_str()
            .expect("tool text"),
    )
    .unwrap()
}

/// Real hotel + production datasource controller + LifeGraph provider + real
/// Memgraph + HTTP MCP. All identities, policy and graph rows are synthetic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires freshly built aiua/life-graph-runner and explicit empty loopback Memgraph fixture"]
async fn real_native_graph_runner_memgraph_mcp_composition() {
    assert_eq!(
        std::env::var("PHILOTIC_GRAPH_TOOL_MEMGRAPH_FIXTURE").as_deref(),
        Ok("1")
    );
    let bin = PathBuf::from(
        std::env::var_os("PHILOTIC_TEST_BIN_DIR").expect("explicit fixture binaries"),
    );
    assert!(
        bin.is_absolute() && bin.join("aiua").is_file() && bin.join("life-graph-runner").is_file()
    );
    let dir = tempfile::Builder::new()
        .prefix("graph-tools-")
        .tempdir_in("/tmp")
        .unwrap();
    let _diagnostics = Diagnostics(dir.path().to_owned());
    // Fixed test port, no environment credentials, no production URI override.
    let graph = neo4rs::Graph::new("127.0.0.1:17687", "", "").unwrap();
    let mut rows = graph
        .execute(neo4rs::query("MATCH (n) RETURN count(n) AS count"))
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>("count")
            .unwrap(),
        0,
        "refuse a nonempty graph fixture"
    );
    drop(rows);
    graph
        .run(neo4rs::query(
            "CREATE (:Goal {id: 'fixture-native-goal', title: 'Synthetic graph recall canary'})",
        ))
        .await
        .unwrap();

    let socket = dir.path().join("hotel.sock");
    let root = dir.path().join("hotel");
    let mut hotel_process = hotel(&bin, &root, "fixture-graph", &socket);
    let mut probe = connect(&socket, "fixture-graph-owner", "probe").await;
    let registered = probe
        .send_request(IpcRequest::RegisterComponent {
            manifest: ansible_mesh_core::storage::ComponentManifest {
                guest_id: "life-fixture".into(),
                role: "life-graph-runner".into(),
                hotel: "fixture-graph".into(),
                command: bin.join("life-graph-runner").to_str().unwrap().into(),
                args: vec![],
                env: HashMap::new(),
                component_config: json!({}),
                auto_start: true,
            },
        })
        .await
        .unwrap();
    assert!(
        matches!(registered, IpcResponse::ComponentRegistered { .. }),
        "{registered:?}"
    );
    let spawn_runner = || {
        let log = std::fs::File::create(dir.path().join("runner.log")).unwrap();
        Process(
            Command::new(bin.join("life-graph-runner"))
                .env_clear()
                .current_dir(dir.path())
                .env("PHILOTIC_HOTEL_SOCKET", &socket)
                .env("PHILOTIC_NODE_ID", "fixture-graph-aiua-01")
                .env("PHILOTIC_LIFE_GRAPH_RUNNER_ID", "life-fixture")
                .env("PHILOTIC_MEMGRAPH_URI", "127.0.0.1:17687")
                .env("PHILOTIC_HEARTBEAT_ENABLED", "0")
                .env("PHILOTIC_LIFE_HYGIENE_ENABLED", "0")
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
        )
    };
    let mut runner = spawn_runner();
    let target = McpRouteTarget::RunnerTool {
        runner_id: "life-fixture".into(),
        tool_name: "life.view.node".into(),
        version: 1,
    };
    let auth = McpAuthScheme::BearerToken {
        grants: vec![
            McpTokenGrant {
                token_id: "fixture-caller".into(),
                vault_ref: "fixture-hash".into(),
                scopes: vec!["node-recall".into()],
                expires_at: None,
                allotment: None,
            },
            McpTokenGrant {
                token_id: "fixture-no-scope".into(),
                vault_ref: "fixture-unscoped".into(),
                scopes: vec![],
                expires_at: None,
                allotment: None,
            },
        ],
    };
    let tool = McpToolSpec {
        name: "node-recall".into(),
        description: "must be replaced by runner".into(),
        input_schema: json!({"type":"object"}),
        inbound_transform: McpInboundTransform::FieldMap {
            action: "life.view.node".into(),
            target: target.clone(),
            mappings: vec![],
        },
        outbound_transform: McpOutboundTransform::PassThrough,
        auth: Some(auth),
        handler: None,
    };
    let reservation = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    let mut config = endpoint_with(vec![tool], None);
    config.endpoint_id = "graph-fixture".into();
    config.owner_agent_id = "fixture-graph-owner".into();
    config.port = port;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    config.preapproval_rules = vec![McpPreapprovalRule {
        action_pattern: "life.view.node".into(),
        target: Some(target),
        approved_by_turn: "synthetic-fixture".into(),
        approved_at: now,
        expires_at: Some(now + 600),
    }];
    let provisioned = probe
        .send_request(IpcRequest::ProvisionMcpEndpoint {
            config: config.clone(),
        })
        .await
        .unwrap();
    assert!(
        matches!(provisioned, IpcResponse::McpEndpointProvisioned { .. }),
        "{provisioned:?}"
    );
    let start_membrane = || {
        let vault = HashMap::from([
            (
                "fixture-hash".into(),
                *blake3::hash(BEARER.as_bytes()).as_bytes(),
            ),
            (
                "fixture-unscoped".into(),
                *blake3::hash(b"SYNTHETIC-UNSCOPED").as_bytes(),
            ),
        ]);
        let mut harness = harness(false, ExposureTier::Local, vault);
        let state = Arc::get_mut(&mut harness.state).unwrap();
        state.node_id = "fixture-graph-aiua-01".into();
        state.guest_id = "mcp-membrane-graph-fixture".into();
        state.runner_tool_socket = Some(socket.to_str().unwrap().into());
        let runtime = membrane::MembraneRuntime::new(
            socket.to_str().unwrap(),
            "mcp-membrane-graph-fixture",
            "fixture-graph-aiua-01",
        )
        .with_inbound_rx(harness.inbound_rx.take().unwrap());
        let state = harness.state.clone();
        let task = Task(tokio::spawn(runtime.run(McpMembrane::new(
            port,
            "mcp-membrane-graph-fixture",
            state.clone(),
        ))));
        (task, state)
    };
    let (mut runtime, _) = start_membrane();
    async fn wait_projection(port: u16, expected: usize) -> Value {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .is_ok()
                {
                    let listed = http_rpc(port, "tools/list", json!({})).await;
                    if listed["result"]["tools"]
                        .as_array()
                        .is_some_and(|tools| tools.len() == expected)
                    {
                        break listed;
                    }
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .expect("native graph projection readiness")
    }
    let listed = wait_projection(port, 1).await;
    assert!(
        listed["result"]["tools"][0]["description"]
            .as_str()
            .unwrap()
            .contains("LifeGraph")
    );
    assert_eq!(
        listed["result"]["tools"][0]["inputSchema"]["required"],
        json!(["id"])
    );
    for bearer in ["WRONG-SYNTHETIC-BEARER", "SYNTHETIC-UNSCOPED"] {
        assert_eq!(
            http_rpc_authenticated(port, "tools/list", json!({}), bearer).await["result"]["tools"],
            json!([])
        );
        assert!(
            http_rpc_authenticated(
                port,
                "tools/call",
                json!({"name":"node-recall","arguments":{"id":"fixture-native-goal"}}),
                bearer
            )
            .await
            .get("error")
            .is_some()
        );
    }
    assert!(
        http_rpc(
            port,
            "tools/call",
            json!({"name":"node-recall","arguments":{"id":"fixture-native-goal","edge_limit":201}})
        )
        .await
        .get("error")
        .is_some()
    );
    assert!(http_rpc(port,"tools/call",json!({"name":"node-recall","arguments":{"id":"fixture-native-goal","api_key":"SYNTHETIC-ARGUMENT-KEY-CANARY"}})).await.get("error").is_some());
    let recalled = result(
        &http_rpc(
            port,
            "tools/call",
            json!({"name":"node-recall","arguments":{"id":"fixture-native-goal","edge_limit":1}}),
        )
        .await,
    );
    assert!(
        recalled
            .to_string()
            .contains("Synthetic graph recall canary"),
        "{recalled}"
    );
    // Kill the actual provider process: persisted history is never liveness.
    runner.stop();
    wait_projection(port, 0).await;
    assert!(
        http_rpc(
            port,
            "tools/call",
            json!({"name":"node-recall","arguments":{"id":"fixture-native-goal"}})
        )
        .await
        .get("error")
        .is_some()
    );
    runner = spawn_runner();
    wait_projection(port, 1).await;
    // Restart the actual hotel on its same disposable SQLite state. Endpoint
    // intent survives; runner definitions and membrane lease must be reacquired.
    runtime.0.abort();
    let _ = (&mut runtime.0).await;
    runner.stop();
    hotel_process.stop();
    hotel_process = hotel(&bin, &root, "fixture-graph", &socket);
    probe = connect(&socket, "fixture-graph-owner", "probe").await;
    let persisted = probe
        .send_request(IpcRequest::GetMcpEndpointStatus {
            endpoint_id: "graph-fixture".into(),
        })
        .await
        .unwrap();
    assert!(
        matches!(persisted,IpcResponse::Standard {ok:true,data:Some(data),..} if data["config"]["tools"][0]["name"]=="node-recall")
    );
    let (restarted_runtime, membrane_state) = start_membrane();
    runtime = restarted_runtime;
    wait_projection(port, 0).await;
    runner = spawn_runner();
    wait_projection(port, 1).await;
    assert!(
        result(
            &http_rpc(
                port,
                "tools/call",
                json!({"name":"node-recall","arguments":{"id":"fixture-native-goal"}})
            )
            .await
        )
        .to_string()
        .contains("Synthetic graph recall canary")
    );
    // Revocation must hold even if a membrane still has a stale cached config.
    let revoked = probe
        .send_request(IpcRequest::RevokeMcpTokenGrant {
            endpoint_id: "graph-fixture".into(),
            owner_agent_id: "fixture-graph-owner".into(),
            token_id: "fixture-caller".into(),
        })
        .await
        .unwrap();
    assert!(
        matches!(revoked, IpcResponse::Standard { ok: true, .. }),
        "{revoked:?}"
    );
    wait_projection(port, 0).await;
    let denied = http_rpc(
        port,
        "tools/call",
        json!({"name":"node-recall","arguments":{"id":"fixture-native-goal"}}),
    )
    .await;
    assert!(
        denied.get("error").is_some(),
        "revoked grant invocation: {denied}"
    );
    membrane_state
        .endpoint_table
        .write()
        .await
        .update(config.clone());
    assert_eq!(
        http_rpc(port, "tools/list", json!({})).await["result"]["tools"],
        json!([]),
        "revoked grant must not reappear from stale membrane policy"
    );
    let denied = http_rpc(
        port,
        "tools/call",
        json!({"name":"node-recall","arguments":{"id":"fixture-native-goal"}}),
    )
    .await;
    assert!(
        denied.get("error").is_some() || denied["result"]["isError"] == true,
        "stale grant invocation: {denied}"
    );
    // A fresh explicit fixture provisioning isolates endpoint revocation from
    // token revocation; confirm a successful read before removing the endpoint.
    let provisioned = probe
        .send_request(IpcRequest::ProvisionMcpEndpoint {
            config: config.clone(),
        })
        .await
        .unwrap();
    assert!(matches!(
        provisioned,
        IpcResponse::McpEndpointProvisioned { .. }
    ));
    wait_projection(port, 1).await;
    assert!(
        result(
            &http_rpc(
                port,
                "tools/call",
                json!({"name":"node-recall","arguments":{"id":"fixture-native-goal"}})
            )
            .await
        )
        .to_string()
        .contains("Synthetic graph recall canary")
    );
    let revoked = probe
        .send_request(IpcRequest::RevokeMcpEndpoint {
            endpoint_id: "graph-fixture".into(),
            owner_agent_id: "fixture-graph-owner".into(),
        })
        .await
        .unwrap();
    assert!(
        matches!(revoked, IpcResponse::McpEndpointProvisioned { .. }),
        "{revoked:?}"
    );
    wait_projection(port, 0).await;
    assert!(
        http_rpc(
            port,
            "tools/call",
            json!({"name":"node-recall","arguments":{"id":"fixture-native-goal"}})
        )
        .await
        .get("error")
        .is_some()
    );
    membrane_state
        .endpoint_table
        .write()
        .await
        .update(config.clone());
    assert_eq!(
        http_rpc(port, "tools/list", json!({})).await["result"]["tools"],
        json!([])
    );
    assert!(
        http_rpc(
            port,
            "tools/call",
            json!({"name":"node-recall","arguments":{"id":"fixture-native-goal"}})
        )
        .await
        .get("error")
        .is_some()
    );
    let persisted = probe
        .send_request(IpcRequest::GetMcpEndpointStatus {
            endpoint_id: "graph-fixture".into(),
        })
        .await
        .unwrap();
    assert!(
        matches!(persisted,IpcResponse::Standard {ok:true,data:Some(data),..} if data["config"].is_null()),
        "revoked endpoint must not persist"
    );
    // Delete only the row created by this fixture after its empty-store check.
    runner.stop();
    runtime.0.abort();
    hotel_process.stop();
    let mut observed = String::new();
    if let Ok(entries) = std::fs::read_dir(root.join("logs")) {
        for entry in entries.flatten() {
            if entry.path().extension().is_some_and(|ext| ext == "log") {
                observed.push_str(&std::fs::read_to_string(entry.path()).unwrap());
            }
        }
    }
    for file in [root.join("process.log"), dir.path().join("runner.log")] {
        observed.push_str(&std::fs::read_to_string(file).unwrap());
    }
    assert!(
        observed.contains("native_graph_transaction"),
        "actual hotel must emit native transaction metadata"
    );
    for field in [
        "admission_duration_us",
        "caller_ref",
        "session_ref",
        "turn_ref",
        "life.view.node",
    ] {
        assert!(observed.contains(field), "missing metadata field {field}");
    }
    assert!(
        observed.contains("\"policy_outcome\":\"allowed\"")
            && observed.contains("\"policy_outcome\":\"denied\"")
    );
    for private in [
        BEARER,
        "SYNTHETIC-UNSCOPED",
        "SYNTHETIC-ARGUMENT-KEY-CANARY",
        "Authorization: Bearer",
        "Synthetic graph recall canary",
    ] {
        assert!(
            !observed.contains(private),
            "credential/header/private graph payload leaked to fixture logs"
        );
    }
    graph.run(neo4rs::query("MATCH (n:Goal {id: 'fixture-native-goal', title: 'Synthetic graph recall canary'}) DELETE n")).await.unwrap();
}

/// Binary matrix for the pre-descriptor hotel/runner and this candidate. The
/// current MCP runtime is qualified separately by the full HTTP composition.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires pinned old/current hotel and LifeGraph binaries plus empty isolated Memgraph"]
async fn real_native_graph_old_new_hotel_runner_binary_matrix() {
    assert_eq!(
        std::env::var("PHILOTIC_GRAPH_TOOL_MEMGRAPH_FIXTURE").as_deref(),
        Ok("1")
    );
    let current =
        PathBuf::from(std::env::var_os("PHILOTIC_TEST_BIN_DIR").expect("current pinned binaries"));
    let old =
        PathBuf::from(std::env::var_os("PHILOTIC_TEST_OLD_BIN_DIR").expect("old pinned binaries"));
    assert!(current.is_absolute() && old.is_absolute() && current != old);
    let graph = neo4rs::Graph::new("127.0.0.1:17687", "", "").unwrap();
    let mut rows = graph
        .execute(neo4rs::query("MATCH (n) RETURN count(n) AS count"))
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>("count")
            .unwrap(),
        0,
        "matrix requires an empty disposable graph"
    );
    drop(rows);
    graph
        .run(neo4rs::query(
            "CREATE (:Goal {id:'fixture-compat-goal',title:'Synthetic compatibility canary'})",
        ))
        .await
        .unwrap();
    for new_hotel in [false, true] {
        for new_runner in [false, true] {
            let dir = tempfile::Builder::new()
                .prefix("graph-compat-")
                .tempdir_in("/tmp")
                .unwrap();
            let _diagnostics = Diagnostics(dir.path().to_owned());
            let socket = dir.path().join("hotel.sock");
            let hotel_name = format!(
                "fixture-compat-{}-{}",
                u8::from(new_hotel),
                u8::from(new_runner)
            );
            let node = format!("{hotel_name}-aiua-01");
            let hotel_bin = if new_hotel { &current } else { &old };
            let runner_bin = if new_runner { &current } else { &old };
            let mut hotel_process =
                hotel(hotel_bin, &dir.path().join("hotel"), &hotel_name, &socket);
            let mut probe = connect(&socket, "fixture-compat-owner", "probe").await;
            let feature = probe
                .send_request(IpcRequest::GetConfig {
                    key: "__runner_tool_protocol__".into(),
                })
                .await
                .unwrap();
            assert_eq!(
                matches!(feature,IpcResponse::ConfigData {value_json:Some(ref value),..} if value=="1"),
                new_hotel,
                "feature negotiation"
            );
            let enrolled = probe
                .send_request(IpcRequest::RegisterComponent {
                    manifest: ansible_mesh_core::storage::ComponentManifest {
                        guest_id: "life-fixture".into(),
                        role: "life-graph-runner".into(),
                        hotel: hotel_name.clone(),
                        command: runner_bin
                            .join("life-graph-runner")
                            .to_str()
                            .unwrap()
                            .into(),
                        args: vec![],
                        env: HashMap::new(),
                        component_config: json!({}),
                        auto_start: true,
                    },
                })
                .await
                .unwrap();
            assert!(matches!(enrolled, IpcResponse::ComponentRegistered { .. }));
            let log = std::fs::File::create(dir.path().join("runner.log")).unwrap();
            let mut runner = Process(
                Command::new(runner_bin.join("life-graph-runner"))
                    .env_clear()
                    .current_dir(dir.path())
                    .env("PHILOTIC_HOTEL_SOCKET", &socket)
                    .env("PHILOTIC_NODE_ID", &node)
                    .env("PHILOTIC_LIFE_GRAPH_RUNNER_ID", "life-fixture")
                    .env("PHILOTIC_MEMGRAPH_URI", "127.0.0.1:17687")
                    .env("PHILOTIC_HEARTBEAT_ENABLED", "0")
                    .env("PHILOTIC_LIFE_HYGIENE_ENABLED", "0")
                    .stdin(Stdio::null())
                    .stdout(log.try_clone().unwrap())
                    .stderr(log)
                    .spawn()
                    .unwrap(),
            );
            // Retry a bounded read until the existing controller has completed
            // startup. These are read-only fixture calls, with no model hop.
            tokio::time::timeout(Duration::from_secs(15),async {
                loop {
                    assert!(runner.0.try_wait().unwrap().is_none(),"runner startup failed for {new_hotel}/{new_runner}");
                    let sent=probe.send_request(IpcRequest::EmitTask {target_node:node.clone(),target_role:"life-graph-runner".into(),target_guest_id:Some("life-fixture".into()),
                        task_json:json!({"tool_name":"life.view.node","arguments":{"id":"fixture-compat-goal","edge_limit":1},"session_id":"compat-legacy","turn_id":"compat-legacy","reply_to":node,"reply_role":"probe","reply_guest_id":"fixture-compat-owner"}).to_string()
                    }).await.unwrap();
                    if matches!(sent,IpcResponse::Standard {ok:true,..})
                        && let Ok(Ok(IpcResponse::InboundTask {task_json,..}))=tokio::time::timeout(Duration::from_millis(250),probe.recv_task()).await
                        && task_json.contains("Synthetic compatibility canary") {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            }).await.expect("legacy read must work in every mixed binary case");
            let target = McpRouteTarget::RunnerTool {
                runner_id: "life-fixture".into(),
                tool_name: "life.view.node".into(),
                version: 1,
            };
            let tool = McpToolSpec {
                name: "node-recall".into(),
                description: "placeholder".into(),
                input_schema: json!({"type":"object"}),
                inbound_transform: McpInboundTransform::FieldMap {
                    action: "life.view.node".into(),
                    target: target.clone(),
                    mappings: vec![],
                },
                outbound_transform: McpOutboundTransform::PassThrough,
                handler: None,
                auth: Some(McpAuthScheme::BearerToken {
                    grants: vec![McpTokenGrant {
                        token_id: "fixture-caller".into(),
                        vault_ref: "synthetic".into(),
                        scopes: vec!["node-recall".into()],
                        expires_at: None,
                        allotment: None,
                    }],
                }),
            };
            let mut config = endpoint_with(vec![tool], None);
            config.endpoint_id = "compat".into();
            config.owner_agent_id = "fixture-compat-owner".into();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            config.preapproval_rules = vec![McpPreapprovalRule {
                action_pattern: "life.view.node".into(),
                target: Some(target),
                approved_by_turn: "synthetic-compatibility".into(),
                approved_at: now,
                expires_at: Some(now + 60),
            }];
            if new_hotel {
                assert!(matches!(
                    probe
                        .send_request(IpcRequest::ProvisionMcpEndpoint { config })
                        .await
                        .unwrap(),
                    IpcResponse::McpEndpointProvisioned { .. }
                ));
                let mut membrane = connect(&socket, "mcp-membrane-compat", "mcp-membrane").await;
                assert!(matches!(
                    membrane
                        .send_request(IpcRequest::AcquireMcpMembraneLease {
                            lease_key: "mcp-membrane-compat".into(),
                            port: 19001
                        })
                        .await
                        .unwrap(),
                    IpcResponse::McpMembraneLease {
                        mcp_granted: true,
                        ..
                    }
                ));
                let definition = membrane
                    .send_request(IpcRequest::GetRunnerTool {
                        endpoint_id: "compat".into(),
                        tool_name: "node-recall".into(),
                        caller_id: "fixture-caller".into(),
                    })
                    .await
                    .unwrap();
                assert_eq!(
                    matches!(definition, IpcResponse::Standard { ok: true, .. }),
                    new_runner,
                    "old runner must never acquire a native descriptor by fallback"
                );
                let invoked = membrane
                    .send_request(IpcRequest::InvokeRunnerTool {
                        endpoint_id: "compat".into(),
                        tool_name: "node-recall".into(),
                        version: 1,
                        arguments: json!({"id":"fixture-compat-goal"}),
                        caller_id: "fixture-caller".into(),
                        session_id: "compat-native".into(),
                        turn_id: "compat-native".into(),
                    })
                    .await
                    .unwrap();
                assert_eq!(
                    matches!(invoked, IpcResponse::Standard { ok: true, .. }),
                    new_runner
                );
                if new_runner {
                    let reply = tokio::time::timeout(Duration::from_secs(5), membrane.recv_task())
                        .await
                        .unwrap()
                        .unwrap();
                    assert!(
                        matches!(reply,IpcResponse::InboundTask {task_json,..} if task_json.contains("Synthetic compatibility canary") && task_json.contains("compat-native"))
                    );
                }
            } else {
                // Positive legacy provisioning control precedes the unknown
                // native target, distinguishing refusal from broken framing.
                let mut legacy = config.clone();
                legacy.endpoint_id = "compat-legacy".into();
                legacy.preapproval_rules.clear();
                legacy.tools[0].inbound_transform = McpInboundTransform::FieldMap {
                    action: "life.view.node".into(),
                    target: McpRouteTarget::Datasource {
                        datasource_id: "life-graph-runner".into(),
                    },
                    mappings: vec![],
                };
                assert!(matches!(
                    probe
                        .send_request(IpcRequest::ProvisionMcpEndpoint { config: legacy })
                        .await
                        .unwrap(),
                    IpcResponse::McpEndpointProvisioned { .. }
                ));
                let native = probe
                    .send_request_with_timeout(
                        IpcRequest::ProvisionMcpEndpoint { config },
                        Duration::from_secs(3),
                    )
                    .await;
                assert!(
                    !matches!(native, Ok(IpcResponse::McpEndpointProvisioned { .. })),
                    "old hotel must refuse the unknown native target"
                );
                let mut observer = connect(&socket, "fixture-compat-observer", "probe").await;
                let state = observer
                    .send_request(IpcRequest::GetMcpEndpointStatus {
                        endpoint_id: "compat".into(),
                    })
                    .await
                    .unwrap();
                assert!(
                    matches!(state,IpcResponse::Standard {ok:true,data:Some(data),..} if data["config"].is_null())
                );
            }
            eprintln!(
                "matrix hotel={} runner={}: legacy recall passed; native={}",
                if new_hotel { "new" } else { "old" },
                if new_runner { "new" } else { "old" },
                new_hotel && new_runner
            );
            runner.stop();
            hotel_process.stop();
        }
    }
    graph.run(neo4rs::query("MATCH (n:Goal {id:'fixture-compat-goal',title:'Synthetic compatibility canary'}) DELETE n")).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicit PHILOTIC_TEST_BIN_DIR containing freshly built aiua and native-memory"]
async fn real_hotels_native_runner_authenticated_mcp_composition() {
    let use_graph = match std::env::var("PHILOTIC_SYNTHETIC_NATIVE_COMPOSITION_MEMGRAPH") {
        Err(std::env::VarError::NotPresent) => false,
        Ok(value) if value == "1" => {
            assert_eq!(
                std::env::var("GITHUB_ACTIONS").as_deref(),
                Ok("true"),
                "Memgraph composition is restricted to the disposable GitHub Actions fixture"
            );
            true
        }
        _ => panic!("Memgraph composition switch must be absent or exactly 1"),
    };
    let bin = PathBuf::from(
        std::env::var_os("PHILOTIC_TEST_BIN_DIR").expect("explicit fixture binary directory"),
    );
    assert!(bin.is_absolute(), "binary directory must be absolute");
    assert!(bin.join("aiua").is_file() && bin.join("native-memory").is_file());
    let dir = tempfile::Builder::new()
        .prefix("nm-compose-")
        .tempdir_in("/tmp")
        .unwrap();
    let _diagnostics = Diagnostics(dir.path().to_owned());
    let _ = tracing_subscriber::fmt()
        .with_env_filter("membrane=debug,membrane_mcp=debug")
        .with_test_writer()
        .try_init();
    // No configurable URI: this is exclusively the existing disposable CI server.
    let graph = if use_graph {
        let graph = neo4rs::Graph::new("127.0.0.1:17687", "", "").unwrap();
        let mut rows = graph
            .execute(neo4rs::query("MATCH (n) RETURN count(n) AS count"))
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>("count")
                .unwrap(),
            0,
            "CI graph must be empty before composition provisioning"
        );
        drop(rows);
        for label in native_memory::memgraph::LABELS {
            graph
                .run(neo4rs::query(&format!(
                    "CREATE CONSTRAINT ON (n:{label}) ASSERT n.key IS UNIQUE"
                )))
                .await
                .unwrap();
        }
        Some(graph)
    } else {
        None
    };
    let a = dir.path().join("a.sock");
    let b = dir.path().join("b.sock");
    let _source = hotel(&bin, &dir.path().join("source"), "fixture-ingress", &a);
    let mut destination = hotel(&bin, &dir.path().join("destination"), "fixture-memory", &b);
    let mut source_probe = connect(&a, "fixture-source-probe", "probe").await;
    let mut destination_probe = connect(&b, "fixture-destination-probe", "probe").await;
    seed(
        &mut source_probe,
        DESTINATION,
        "fixture-memory",
        RUNNER,
        native_memory::ROLE,
        &b,
    )
    .await;
    seed(
        &mut destination_probe,
        SOURCE,
        "fixture-ingress",
        GUEST,
        "mcp-membrane",
        &a,
    )
    .await;

    let request_key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let response_key = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
    let grant = native_memory::Grant {
        issuer: SOURCE.into(),
        reply_node: SOURCE.into(),
        reply_guest: GUEST.into(),
        principal: "synthetic-alice".into(),
        owner: "synthetic-alice".into(),
        namespace: "fixture-notes".into(),
        destination: DESTINATION.into(),
        runner: RUNNER.into(),
        operations: vec!["remember".into(), "recall".into()],
        revision: 1,
        expires_at: u64::MAX,
        revoked: false,
    };
    let grants = std::collections::BTreeMap::from([("fixture-caller".to_string(), grant)]);
    let issuer = dir.path().join("issuer.json");
    private_json(
        &issuer,
        &json!({"issuer":SOURCE,"key_id":"fixture-request-key","signing_seed":vec![7;32],"grants":grants,
        "response_verifiers":[{"destination":DESTINATION,"runner":RUNNER,"key_id":"fixture-response-key","public_key":response_key.verifying_key().to_bytes()}]}),
    );
    let policy = dir.path().join("policy.json");
    private_json(
        &policy,
        &native_memory::Policy {
            issuers: std::collections::BTreeMap::from([(
                SOURCE.to_string(),
                native_memory::TrustedIssuer {
                    key_id: "fixture-request-key".into(),
                    public_key: request_key.verifying_key().to_bytes(),
                },
            )]),
            grants,
        },
    );
    let signer = dir.path().join("response-signer.json");
    private_json(
        &signer,
        &json!({"key_id":"fixture-response-key","signing_seed":vec![8;32]}),
    );
    let database = dir.path().join("memory.db");
    let graph_config = dir.path().join("synthetic-memgraph.json");
    if use_graph {
        private_json(
            &graph_config,
            &json!({"uri":"127.0.0.1:17687","username":"","password":""}),
        );
    }
    let spawn_runner = || {
        let log = std::fs::File::create(dir.path().join("runner.log")).unwrap();
        let (backend_arg, backend_path) = if use_graph {
            ("--memgraph-config", &graph_config)
        } else {
            ("--sqlite-fixture", &database)
        };
        Process(
            Command::new(bin.join("native-memory"))
                .env_clear()
                .current_dir(dir.path())
                .arg("--socket")
                .arg(&b)
                .args(["--node", DESTINATION, "--guest", RUNNER])
                .arg(backend_arg)
                .arg(backend_path)
                .arg("--policy")
                .arg(&policy)
                .arg("--response-signing-config")
                .arg(&signer)
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
        )
    };
    let mut runner = spawn_runner();
    let vault = HashMap::from([(
        "fixture-hash".into(),
        *blake3::hash(BEARER.as_bytes()).as_bytes(),
    )]);
    let mut h = harness(false, ExposureTier::Local, vault);
    let state = Arc::get_mut(&mut h.state).unwrap();
    state.native_memory_config = Some(issuer);
    state.node_id = SOURCE.into();
    state.guest_id = GUEST.into();
    let auth = McpAuthScheme::BearerToken {
        grants: vec![McpTokenGrant {
            token_id: "fixture-caller".into(),
            vault_ref: "fixture-hash".into(),
            scopes: vec![],
            expires_at: None,
            allotment: None,
        }],
    };
    let tools = ["remember", "recall"]
        .into_iter()
        .map(|op| McpToolSpec {
            name: format!("native-memory.{op}"),
            description: "Synthetic fixture".into(),
            input_schema: json!({"type":"object"}),
            inbound_transform: McpInboundTransform::FieldMap {
                action: format!("native-memory.{op}"),
                target: McpRouteTarget::NativeMemory {
                    runner_id: RUNNER.into(),
                    target_node: DESTINATION.into(),
                },
                mappings: vec![],
            },
            outbound_transform: McpOutboundTransform::PassThrough,
            auth: Some(auth.clone()),
            handler: None,
        })
        .collect();
    let reservation = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    // Use the hotel's typed, persisted provisioning path, not endpoint-table injection.
    // Smoke hotels deliberately disable guest materialization; the production guest
    // runtime below replays this config through GetMcpEndpointStatus on startup.
    let mut config = endpoint_with(tools, None);
    config.endpoint_id = "fixture".into();
    config.owner_agent_id = "fixture-source-probe".into();
    config.port = port;
    config.preapproval_rules = ["remember", "recall"]
        .into_iter()
        .map(|op| McpPreapprovalRule {
            action_pattern: format!("native-memory.{op}"),
            target: None,
            approved_by_turn: "synthetic-provision".into(),
            approved_at: 0,
            expires_at: None,
        })
        .collect();
    let mut legacy = config.clone();
    legacy.endpoint_id = "unrelated-fixture".into();
    legacy.tools[0].inbound_transform = McpInboundTransform::FieldMap {
        action: "synthetic.legacy".into(),
        target: McpRouteTarget::Datasource {
            datasource_id: "synthetic-legacy".into(),
        },
        mappings: vec![],
    };
    legacy.tools.truncate(1);
    legacy.preapproval_rules.clear();
    if std::env::var("PHILOTIC_EXPECT_NATIVE_ROUTE_REJECTION").as_deref() == Ok("1") {
        // Negative control for an original B7 binary, not merely its source enum.
        // Establish working IPC/provisioning before sending the unsupported variant.
        assert!(matches!(
            source_probe
                .send_request(IpcRequest::ProvisionMcpEndpoint { config: legacy })
                .await
                .unwrap(),
            IpcResponse::McpEndpointProvisioned { .. }
        ));
        let rejected = source_probe
            .send_request_with_timeout(
                IpcRequest::ProvisionMcpEndpoint { config },
                Duration::from_secs(3),
            )
            .await;
        assert!(
            matches!(rejected,
            Ok(IpcResponse::Standard { ok: false, ref code, ref message, .. })
                if code == "MALFORMED_PAYLOAD" && message.contains("unknown variant `native_memory`")),
            "negative control requires explicit native route decoding rejection, not timeout/disconnection: {rejected:?}"
        );
        let status = source_probe
            .send_request(IpcRequest::GetMcpEndpointStatus {
                endpoint_id: "fixture".into(),
            })
            .await
            .unwrap();
        assert!(
            matches!(status, IpcResponse::Standard { ok: true, data: Some(ref data), .. }
            if data["config"].is_null()),
            "rejected native config must not persist: {status:?}"
        );
        eprintln!("Original B7 binary rejected native_memory after accepting legacy endpoint");
        return;
    }
    let mut wrong_owner = config.clone();
    wrong_owner.owner_agent_id = "not-the-registered-owner".into();
    assert!(matches!(
        source_probe
            .send_request(IpcRequest::ProvisionMcpEndpoint {
                config: wrong_owner
            })
            .await
            .unwrap(),
        IpcResponse::Standard { ok: false, .. }
    ));
    for endpoint in [&legacy, &config] {
        let result = source_probe
            .send_request(IpcRequest::ProvisionMcpEndpoint {
                config: endpoint.clone(),
            })
            .await
            .unwrap();
        assert!(
            matches!(
                result,
                IpcResponse::McpEndpointProvisioned {
                    materialized: false,
                    ..
                }
            ),
            "smoke provision must persist config without spawning guests: {result:?}"
        );
    }
    let mut incarnations = std::collections::BTreeSet::new();
    for endpoint in [&legacy, &config] {
        let stored = source_probe
            .send_request(IpcRequest::GetMcpEndpointStatus {
                endpoint_id: endpoint.endpoint_id.clone(),
            })
            .await
            .unwrap();
        let IpcResponse::Standard {
            ok: true,
            data: Some(data),
            ..
        } = stored
        else {
            panic!("expected persisted endpoint config: {stored:?}")
        };
        let persisted =
            serde_json::from_value::<McpEndpointConfig>(data["config"].clone()).unwrap();
        let incarnation = persisted
            .approval_incarnation
            .as_deref()
            .expect("hotel-minted endpoint incarnation");
        assert!(uuid::Uuid::parse_str(incarnation).is_ok());
        assert!(
            incarnations.insert(incarnation.to_owned()),
            "each endpoint has a distinct incarnation"
        );
        let mut expected = endpoint.clone();
        expected.approval_incarnation = persisted.approval_incarnation.clone();
        assert_eq!(
            persisted, expected,
            "all caller-declared endpoint fields remain unchanged"
        );
    }
    let runtime = membrane::MembraneRuntime::new(a.to_str().unwrap(), GUEST, SOURCE)
        .with_inbound_rx(h.inbound_rx.take().unwrap());
    let _runtime = Task(tokio::spawn(runtime.run(McpMembrane::new(
        port,
        GUEST,
        h.state.clone(),
    ))));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(
                runner.0.try_wait().unwrap().is_none(),
                "native runner exited before readiness"
            );
            if (use_graph || database.exists())
                && tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("composition readiness");
    // Runner creates its DB immediately before registering over IPC.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let initialized = tokio::time::timeout(Duration::from_secs(3), http_rpc(port, "initialize",
        json!({"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"synthetic-composition","version":"1"}}))).await.unwrap();
    assert_eq!(initialized["result"]["protocolVersion"], "2025-03-26");
    let listed = tokio::time::timeout(
        Duration::from_secs(3),
        http_rpc(port, "tools/list", json!({})),
    )
    .await
    .unwrap();
    let names: Vec<_> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(names.contains(&"native-memory.remember") && names.contains(&"native-memory.recall"));
    let remember = json!({"operation":"remember","id":"fixture-note","subject":"practice","content":"synthetic blue notebook","source":"synthetic://composition","source_version":"1","lineage":"fixture-source"});
    let first = tokio::time::timeout(
        Duration::from_secs(8),
        http(port, "remember", "fixture-event", remember.clone()),
    )
    .await
    .unwrap();
    let first = result(&first);
    assert_eq!(first["receipt"]["status"], "committed");
    runner.stop();
    runner = spawn_runner();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(runner.0.try_wait().unwrap().is_none());
    let retry = tokio::time::timeout(
        Duration::from_secs(8),
        http(port, "remember", "fixture-event", remember),
    )
    .await
    .unwrap();
    assert_eq!(
        result(&retry),
        first,
        "durable idempotent result after runner restart"
    );
    let recall = json!({"operation":"recall","query":"notebook","limit":5});
    let found = tokio::time::timeout(
        Duration::from_secs(8),
        http(port, "recall", "fixture-recall", recall.clone()),
    )
    .await
    .unwrap();
    let found = result(&found);
    assert_eq!(found["observations"].as_array().unwrap().len(), 1);
    assert_eq!(
        found["observations"][0]["authenticated_writer"],
        "synthetic-alice"
    );
    assert_eq!(
        found["observations"][0]["content"],
        "synthetic blue notebook"
    );
    assert_eq!(
        found["observations"][0]["source"],
        "synthetic://composition"
    );
    assert_eq!(found["observations"][0]["source_version"], "1");
    assert_eq!(found["observations"][0]["lineage"], "fixture-source");
    destination.stop();
    let unavailable = tokio::time::timeout(
        Duration::from_secs(2),
        http(port, "recall", "fixture-destination-stopped", recall),
    )
    .await;
    if let Ok(response) = unavailable {
        assert!(
            response.get("error").is_some() || response["result"]["isError"] == true,
            "stopped destination must never produce a successful result: {response}"
        );
    }
    runner.stop();
    if let Some(graph) = graph {
        let mut rows = graph
            .execute(neo4rs::query(
                "MATCH (n:NativeMemoryObservation) RETURN n.body AS body",
            ))
            .await
            .unwrap();
        let mut count = 0;
        while let Some(row) = rows.next().await.unwrap() {
            let body: String = row.get("body").unwrap();
            assert!(!body.contains(BEARER));
            count += 1;
        }
        assert_eq!(count, 1, "retry must not duplicate the observation");
        drop(rows);
        // Only remove this adapter's synthetic fixture, leaving the service alive
        // and empty for the next existing CI acceptance test.
        for label in native_memory::memgraph::LABELS {
            graph
                .run(neo4rs::query(&format!("MATCH (n:{label}) DELETE n")))
                .await
                .unwrap();
            graph
                .run(neo4rs::query(&format!(
                    "DROP CONSTRAINT ON (n:{label}) ASSERT n.key IS UNIQUE"
                )))
                .await
                .unwrap();
        }
        let mut rows = graph
            .execute(neo4rs::query("MATCH (n) RETURN count(n) AS count"))
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>("count")
                .unwrap(),
            0
        );
    } else {
        let bytes = std::fs::read(database).unwrap();
        assert!(!bytes.windows(BEARER.len()).any(|w| w == BEARER.as_bytes()));
    }
}
