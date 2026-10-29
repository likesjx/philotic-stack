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
        for hotel in ["source", "destination"] {
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
    std::fs::create_dir(root).unwrap();
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
    let body = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string();
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {BEARER}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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
    h.set_endpoint(endpoint_with(tools, None)).await;
    let reservation = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
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
