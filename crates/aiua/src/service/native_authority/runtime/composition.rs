//! Explicit CI-only composition with real guests, hotel IPC, private coordinator,
//! TLS mock issuer and disposable Memgraph. No installed configuration is read.
use super::*;
use ansible_mesh_core::{
    domain::GraphDomain, materializer::Materializer, sqlite_storage::SqliteGraphStorage,
};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct SharedMaterializer(
    Arc<tokio::sync::Mutex<crate::service::guest_manager::LocalProcessMaterializer>>,
);
#[async_trait::async_trait]
impl Materializer for SharedMaterializer {
    async fn spawn_guest(&mut self, guest: &str, config: &Value) -> Result<String> {
        self.0.lock().await.spawn_guest(guest, config).await
    }
    async fn reclaim_guest(&mut self, guest: &str) -> Result<()> {
        self.0.lock().await.reclaim_guest(guest).await
    }
    async fn check_status(&mut self, guest: &str, active: &str) -> Result<bool> {
        self.0.lock().await.check_status(guest, active).await
    }
}

fn json_file(path: &Path, value: Value) {
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}
fn port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn artifact(source: &Path, destination: &Path) -> (String, String) {
    std::fs::copy(source, destination).unwrap();
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o555)).unwrap();
    (
        destination.to_str().unwrap().into(),
        format!("{:x}", Sha256::digest(std::fs::read(destination).unwrap())),
    )
}
async fn rpc(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    name: &str,
    event: &str,
    command: Value,
) -> Value {
    client
        .post(url)
        .bearer_auth(token)
        .json(&json!({"jsonrpc":"2.0","id":event,"method":"tools/call",
        "params":{"name":name,"arguments":{"event":event,"command":command}}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
fn result(value: Value) -> Value {
    assert!(value.get("error").is_none(), "{value}");
    assert_ne!(value["result"]["isError"], true, "{value}");
    serde_json::from_str(
        value["result"]["content"][0]["text"]
            .as_str()
            .expect("tool result"),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit disposable CI Memgraph, Node and freshly built guest binaries"]
async fn same_hotel_oauth_http_capture_recall_and_coordinated_revocation() {
    assert_eq!(std::env::var("GITHUB_ACTIONS").as_deref(), Ok("true"));
    assert_eq!(
        std::env::var("PHILOTIC_NATIVE_OAUTH_COMPOSITION").as_deref(),
        Ok("1")
    );
    let binaries = PathBuf::from(std::env::var("PHILOTIC_TEST_BIN_DIR").unwrap());
    let node = PathBuf::from(std::env::var("NODE_TEST_EXECUTABLE").unwrap())
        .canonicalize()
        .unwrap();
    let dir = tempfile::Builder::new()
        .prefix("native-oauth-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = dir.path();
    let hotel = "native-fixture";
    let membrane = "mcp-membrane-native-fixture";
    let runner = "native-runner";
    let subject = "00000000-0000-4000-8000-000000000001";
    let resource = "https://native.example/mcp";
    let issuer_port = port();
    let mcp_port = port();
    let issuer = format!("https://127.0.0.1:{issuer_port}");
    let token = "a".repeat(43);
    let secret = "synthetic-introspection-secret-not-a-real-credential";
    let socket = root.join("hotel.sock");
    let control = root.join("control.sock");
    let issuer_socket = root.join("issuer.sock");
    let cert = root.join("issuer.crt");
    let key = root.join("issuer.key");
    assert!(
        std::process::Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "1",
                "-subj",
                "/CN=127.0.0.1",
                "-addext",
                "subjectAltName=IP:127.0.0.1",
                "-keyout"
            ])
            .arg(&key)
            .arg("-out")
            .arg(&cert)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    for path in [&key, &cert] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let client = reqwest::Client::builder()
        .add_root_certificate(
            reqwest::Certificate::from_pem(&std::fs::read(&cert).unwrap()).unwrap(),
        )
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let request_key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let response_key = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
    let grants = json!({"fixture":{"issuer":hotel,"reply_node":hotel,"reply_guest":membrane,"principal":subject,"owner":subject,
        "namespace":"native-oauth-synthetic","destination":hotel,"runner":runner,"operations":["remember","recall"],"revision":1,"expires_at":now+600,"revoked":false}});
    let policy = root.join("policy.json");
    let ingress = root.join("ingress.json");
    let signer = root.join("signer.json");
    let graph = root.join("graph.json");
    json_file(
        &policy,
        json!({"issuers":{(hotel):{"key_id":"fixture-key","public_key":request_key.verifying_key().to_bytes()}},"grants":grants}),
    );
    json_file(
        &ingress,
        json!({"issuer":hotel,"key_id":"fixture-key","signing_seed":vec![7;32],"grants":grants,
        "response_verifiers":[{"destination":hotel,"runner":runner,"key_id":"fixture-result","public_key":response_key.verifying_key().to_bytes()}],
        "oauth":{"issuer":issuer,"resource":resource,"metadata_url":"https://native.example/.well-known/oauth-protected-resource/mcp","authority_socket":control,"grant_id":"fixture"}}),
    );
    json_file(
        &signer,
        json!({"key_id":"fixture-result","signing_seed":vec![8;32]}),
    );
    json_file(
        &graph,
        json!({"uri":"127.0.0.1:17687","username":"","password":""}),
    );
    let issuer_config = root.join("issuer.json");
    json_file(
        &issuer_config,
        json!({"port":issuer_port,"key":key,"cert":cert,"issuer":issuer,"resource":resource,"subject":subject,"token":token,"secret":secret}),
    );
    let bundle = root.join("mock-issuer.cjs");
    std::fs::write(&bundle, include_bytes!("mock_issuer.cjs")).unwrap();
    std::fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o444)).unwrap();
    let (mcp_bin, mcp_sha) = artifact(&binaries.join("membrane-mcp"), &root.join("membrane"));
    let (runner_bin, runner_sha) = artifact(&binaries.join("native-memory"), &root.join("runner"));
    let (node_bin, node_sha) = artifact(&node, &root.join("node"));
    let workers = vec![
        json!({"guest":membrane,"agent":"fixture-membrane","roles":["membrane"],"command":mcp_bin,"sha256":mcp_sha,
          "args":["--node-id",hotel,"--guest-id",membrane,"--ipc-socket",socket,"--port",mcp_port.to_string(),"--native-memory-config",ingress],"environment":{}}),
        json!({"guest":runner,"agent":"fixture-runner","roles":["native-memory-runner"],"command":runner_bin,"sha256":runner_sha,
          "args":["--node",hotel,"--guest",runner,"--socket",socket,"--policy",policy,"--response-signing-config",signer,"--memgraph-config",graph,"--authority-socket",control],"environment":{}}),
        json!({"guest":"issuer","agent":"fixture-issuer","roles":["issuer"],"command":node_bin,"sha256":node_sha,
          "args":[bundle,issuer_config],"environment":{"PERSONAL_OAUTH_NATIVE_MEMORY_RESOURCE":resource},
          "artifacts":[{"path":bundle,"sha256":format!("{:x}",Sha256::digest(std::fs::read(&bundle).unwrap()))}]}),
    ];
    let journal = root.join("journal");
    std::fs::write(&journal, b"clean\n").unwrap();
    let secret_file = root.join("secret");
    std::fs::write(&secret_file, secret).unwrap();
    for path in [&journal, &secret_file] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let config = root.join("runtime.json");
    json_file(
        &config,
        json!({"hotel":hotel,"owner_id":"native-fixture-owner","membrane":membrane,"runner":runner,"issuer_guest":"issuer",
        "control_socket":control,"issuer_socket":issuer_socket,"journal":journal,"policy_file":policy,"introspection_client":"fixture-client",
        "introspection_secret_file":secret_file,"introspection_ca_file":cert,
        "binding":{"issuer":issuer,"resource":resource,"client_id":"fixture-dot","subject":subject,"grant_version":"0:7","owner":subject,"namespace":"native-oauth-synthetic","operations":["remember","recall"]},"workers":workers}),
    );
    let runtime = Runtime::load(&config, hotel).await.unwrap();
    let store = SqliteGraphStorage::open(":memory:").unwrap();
    let domain = Arc::new(GraphDomain::new(Arc::new(store.adapter())));
    domain
        .upsert_hotel(&ansible_mesh_core::storage::HotelRecord {
            hotel_name: hotel.into(),
            capabilities: serde_json::from_value(json!({"node_id":hotel,"roles":[]})).unwrap(),
            mesh_host: None,
            mesh_port: 0,
            blob_port: 0,
            execution_port: 0,
            ipc_socket_path: socket.to_str().unwrap().into(),
            active_pid: None,
        })
        .unwrap();
    let materializer = Arc::new(tokio::sync::Mutex::new(
        crate::service::guest_manager::LocalProcessMaterializer::new(":memory:")
            .with_hotel_socket(socket.to_str().unwrap())
            .with_native_authority(runtime),
    ));
    let manager = Arc::new(crate::service::guest_manager::GuestManager::new(
        hotel,
        domain.clone(),
        Box::new(SharedMaterializer(materializer.clone())),
    ));
    for guest in ["issuer", runner] {
        let worker = workers.iter().find(|w| w["guest"] == guest).unwrap();
        domain
            .upsert_guest(&ansible_mesh_core::storage::GuestRecord {
                hotel_name: hotel.into(),
                guest_id: guest.into(),
                role: guest.into(),
                config_json: worker.to_string(),
                is_active: true,
                active_pid: None,
                last_active_at: None,
            })
            .unwrap();
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let server = crate::service::ipc::IpcServer::new(socket.to_str().unwrap(), hotel, tx, domain)
        .with_materialization_requester(manager.clone());
    let server = tokio::spawn(async move { server.run().await });
    let mut probe = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(c) = PhiloticClient::connect_at(
                socket.to_str().unwrap(),
                GuestIdentity {
                    guest_id: "fixture-owner".into(),
                    role: "operator".into(),
                    supported_tools: vec![],
                },
            )
            .await
            {
                break c;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    for (guest, role) in [(membrane, "mcp-membrane"), (runner, "native-memory-runner")] {
        assert!(matches!(
            probe
                .send_request(IpcRequest::SeedRemoteIncarnation {
                    node_id: hotel.into(),
                    hotel_id: hotel.into(),
                    incarnation_id: format!("{hotel}:{guest}"),
                    target_role: role.into(),
                    socket_path: Some(socket.to_str().unwrap().into())
                })
                .await
                .unwrap(),
            IpcResponse::Standard { ok: true, .. }
        ));
    }
    let tools:Vec<Value>=["remember","recall"].into_iter().map(|op|json!({"name":format!("native-memory.{op}"),"description":"Synthetic capture",
        "input_schema":{"type":"object"},"inbound_transform":{"kind":"field_map","action":format!("native-memory.{op}"),"target":{"kind":"native_memory","runner_id":runner,"target_node":hotel},"mappings":[]},
        "outbound_transform":{"kind":"pass_through"},"auth":{"scheme":"bearer_token","grants":[{"token_id":"fixture","vault_ref":"unused-in-oauth-mode","scopes":[]}]}})).collect();
    let endpoint=serde_json::from_value(json!({"endpoint_id":"native-fixture","owner_agent_id":"fixture-owner","port":mcp_port,"path":null,"exposure":"local","tools":tools,"default_auth":null,"allow_unauthenticated":false,
        "preapproval_rules":[
            {"action_pattern":"native-memory.remember","target":null,"approved_by_turn":"synthetic","approved_at":0,"expires_at":null},
            {"action_pattern":"native-memory.recall","target":null,"approved_by_turn":"synthetic","approved_at":0,"expires_at":null}
        ],"updated_at":0})).unwrap();
    for guest in ["issuer", runner] {
        assert!(manager.ensure_guest_active(guest).await.unwrap());
    }
    assert!(matches!(
        probe
            .send_request(IpcRequest::ProvisionMcpEndpoint { config: endpoint })
            .await
            .unwrap(),
        IpcResponse::McpEndpointProvisioned {
            materialized: true,
            ..
        }
    ));
    let url = format!("http://127.0.0.1:{mcp_port}/mcp");
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if client
                .get(format!("http://127.0.0.1:{mcp_port}/health"))
                .send()
                .await
                .is_ok()
                && client
                    .get(format!("{issuer}/oauth/introspect"))
                    .send()
                    .await
                    .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let event = uuid::Uuid::new_v4().to_string();
    let remember = json!({"operation":"remember","id":event,"subject":subject,"content":"Synthetic violet lantern","source":"synthetic://oauth","source_version":"1","lineage":"independent"});
    for rejected in [
        "u".repeat(43),
        "s".repeat(43),
        "r".repeat(43),
        "c".repeat(43),
        "e".repeat(43),
        "v".repeat(43),
        "z".repeat(43),
    ] {
        let denied = rpc(
            &client,
            &url,
            &rejected,
            "native-memory.remember",
            &format!("deny-{rejected}"),
            remember.clone(),
        )
        .await;
        assert_eq!(denied["error"]["code"], -32001, "{denied}");
    }
    let saved = result(
        rpc(
            &client,
            &url,
            &token,
            "native-memory.remember",
            &event,
            remember.clone(),
        )
        .await,
    );
    assert_eq!(saved["receipt"]["status"], "committed");
    let retry = result(
        rpc(
            &client,
            &url,
            &token,
            "native-memory.remember",
            &event,
            remember,
        )
        .await,
    );
    assert_eq!(saved["receipt"], retry["receipt"]);
    let recalled = result(
        rpc(
            &client,
            &url,
            &token,
            "native-memory.recall",
            "recall",
            json!({"operation":"recall","query":"violet","limit":10}),
        )
        .await,
    );
    assert!(
        recalled["observations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["id"] == event)
    );
    assert!(
        client
            .post(format!("{issuer}/test/revoke"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert_eq!(
        rpc(
            &client,
            &url,
            &token,
            "native-memory.recall",
            "revoked",
            json!({"operation":"recall","query":"violet","limit":10})
        )
        .await["error"]["code"],
        -32001
    );
    for guest in [membrane, runner, "issuer"] {
        materializer
            .lock()
            .await
            .reclaim_guest(guest)
            .await
            .unwrap();
    }
    server.abort();
    drain.abort();
}
