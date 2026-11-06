//! Explicit CI-only composition with real guests, hotel IPC, private coordinator,
//! TLS mock issuer and disposable Memgraph. No installed configuration is read.
use super::*;
use ansible_mesh_core::{
    domain::GraphDomain, materializer::Materializer, sqlite_storage::SqliteGraphStorage,
};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::os::unix::process::CommandExt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct FixtureProcess(std::process::Child);
impl Drop for FixtureProcess {
    fn drop(&mut self) {
        // Each synthetic process owns a separate group, including hotel children.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

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
// Cold-start trust qualification only. The caller owns a private CI TempDir;
// this does not perform a CA transition or establish a writer barrier.
fn fixture_openssl(mut command: std::process::Command, capture_stderr: bool) -> Result<(std::process::ExitStatus, Vec<u8>)> {
    let stderr = if capture_stderr { std::process::Stdio::piped() } else { std::process::Stdio::null() };
    command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(stderr);
    let mut child = command.spawn()?;
    let capture = if capture_stderr {
        use std::io::Read as _;
        let stream = child.stderr.take().expect("piped stderr");
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stream.take(16385).read_to_end(&mut bytes).map(|_| bytes);
            let _ = sender.send(result);
        });
        Some(receiver)
    } else { None };
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let bytes = match capture {
                    Some(receiver) => receiver.recv_timeout(Duration::from_secs(2))??,
                    None => Vec::new(),
                };
                ensure!(bytes.len() <= 16384, "synthetic openssl output too large");
                return Ok((status, bytes));
            },
            Ok(None) if std::time::Instant::now() < deadline => {},
            _ => {
                // Kill may race with normal exit; always attempt a bounded
                // reap and fail if its outcome cannot be confirmed.
                let _ = child.kill();
                let reap_deadline = std::time::Instant::now() + Duration::from_secs(5);
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) => anyhow::bail!("synthetic openssl interrupted"),
                        Ok(None) if std::time::Instant::now() < reap_deadline => std::thread::sleep(Duration::from_millis(25)),
                        _ => anyhow::bail!("synthetic openssl reap unconfirmed"),
                    }
                }
            },
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn separate_authority_tls(root: &Path) -> Result<Value> {
    ensure!(root.parent() == Some(Path::new("/tmp")) && root.file_name().and_then(|s| s.to_str()).is_some_and(|s| s.starts_with("native-oauth-")), "synthetic fixture root required");
    let ca = root.join("separate-ca.crt");
    let ca_key = root.join("separate-ca.key");
    let mut command = std::process::Command::new("/usr/bin/openssl");
    command.args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=synthetic-authority-ca", "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign", "-keyout"]).arg(&ca_key).arg("-out").arg(&ca);
    ensure!(fixture_openssl(command, false)?.0.success(), "synthetic CA generation failed");
    std::fs::set_permissions(&ca_key, std::fs::Permissions::from_mode(0o600))?;
    let mut values = json!({"authorityCa":ca});
    for (index, role) in ["authority", "website", "management"].into_iter().enumerate() {
        let key = root.join(format!("separate-{role}.key"));
        let csr = root.join(format!("separate-{role}.csr"));
        let cert = root.join(format!("separate-{role}.crt"));
        let ext = root.join(format!("separate-{role}.ext"));
        let usage = if role == "authority" { "extendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1\n" } else { "extendedKeyUsage=clientAuth\n" };
        std::fs::write(&ext, format!("basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\n{usage}"))?;
        let mut command = std::process::Command::new("/usr/bin/openssl");
        command.args(["req", "-new", "-newkey", "rsa:2048", "-nodes", "-subj"]).arg(format!("/CN=synthetic-{role}")).arg("-keyout").arg(&key).arg("-out").arg(&csr);
        ensure!(fixture_openssl(command, false)?.0.success(), "synthetic leaf request failed");
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600))?;
        let mut command = std::process::Command::new("/usr/bin/openssl");
        command.args(["x509", "-req", "-in"]).arg(&csr).arg("-CA").arg(&ca).arg("-CAkey").arg(&ca_key).arg("-set_serial").arg((index+1).to_string()).args(["-days", "1", "-extfile"]).arg(&ext).arg("-out").arg(&cert);
        ensure!(fixture_openssl(command, false)?.0.success(), "synthetic leaf signing failed");
        values[format!("{role}Cert")] = json!(cert);
        values[format!("{role}Key")] = json!(key);
    }
    Ok(values)
}
fn live_guest_snapshot(db: &Path, hotel: &str, guests: [&str; 3]) -> [(String, String, String); 3] {
    let storage = SqliteGraphStorage::open(db).unwrap();
    let graph = GraphDomain::new(Arc::new(storage.adapter()));
    guests.map(|guest| {
        let record = graph.get_guest(hotel, guest).unwrap().unwrap();
        assert!(record.is_active);
        let pid = record.active_pid.expect("live guest PID");
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let start = stat
            .rsplit_once(") ")
            .unwrap()
            .1
            .split_whitespace()
            .nth(19)
            .unwrap()
            .to_owned();
        (pid, start, record.config_json)
    })
}

async fn denied_renewal_phase(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    subject: &str,
    code: i64,
    message: &str,
) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    let value = rpc(client,url,token,"native-memory.remember",&id,
        json!({"operation":"remember","id":id,"subject":subject,"content":"Synthetic denied violet lantern",
        "source":"synthetic://renewal-denied","source_version":"1","lineage":"independent"})).await;
    assert_eq!(value["error"]["code"], code, "{value}");
    assert_eq!(value["error"]["message"], message, "{value}");
    id
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
    composition(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit CI: packaged aiua/guests, actual issuer bundle, disposable Mongo and Memgraph"]
async fn packaged_hotel_real_issuer_native_capture_and_revocation() {
    composition(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit CI: exact packaged runtime and website, disposable databases"]
async fn packaged_hotel_real_issuer_expired_preapproval_renewal() {
    assert_eq!(
        std::env::var("PHILOTIC_TEST_PREAPPROVAL_RENEWAL").as_deref(),
        Ok("1")
    );
    composition(true).await;
}

async fn composition(real: bool) {
    let renewal = std::env::var("PHILOTIC_TEST_PREAPPROVAL_RENEWAL").as_deref() == Ok("1");
    assert!(
        !renewal || real,
        "renewal qualification requires packaged real issuer"
    );
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
    let bridge = std::env::var("PHILOTIC_MCP_TLS_BIND").ok();
    let mcp_port = if bridge.is_some() { 19101 } else { port() };
    let issuer = format!("https://127.0.0.1:{issuer_port}");
    let mut token = "a".repeat(43);
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
    let mut client = reqwest::Client::builder()
        .add_root_certificate(
            reqwest::Certificate::from_pem(&std::fs::read(&cert).unwrap()).unwrap(),
        )
        .timeout(Duration::from_secs(15));
    if bridge.is_some() {
        client = client.add_root_certificate(
            reqwest::Certificate::from_pem(
                &std::fs::read("/fixture/bridge/public/ca.pem").unwrap(),
            )
            .unwrap(),
        );
    }
    let client = client.build().unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let request_key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let response_key = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
    let grants = json!({"fixture":{"issuer":hotel,"reply_node":hotel,"reply_guest":membrane,"principal":subject,"owner":subject,
        "namespace":"native-oauth-synthetic","destination":hotel,"runner":runner,"operations":["remember","recall"],"revision":1,"expires_at":if renewal { now-1 } else { now+600 },"revoked":false}});
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
    let authority_port = port();
    let mut issuer_settings = json!({"port":issuer_port,"authorityPort":authority_port,"key":key,"cert":cert,"issuer":issuer,"resource":resource,"subject":subject,"token":token,"secret":secret});
    let separate_tls = std::env::var("PHILOTIC_TEST_SEPARATE_AUTHORITY_TLS").as_deref() == Ok("1");
    assert!(!separate_tls || (real && !renewal), "cold-start synthetic real fixture only");
    let separate = if separate_tls { Some(separate_authority_tls(root).unwrap()) } else { None };
    if let Some(ref fields) = separate {
        for (name, value) in fields.as_object().unwrap() { issuer_settings[name] = value.clone(); }
    }
    json_file(&issuer_config, issuer_settings);
    let bundle = root.join("mock-issuer.cjs");
    std::fs::write(&bundle, include_bytes!("mock_issuer.cjs")).unwrap();
    std::fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o444)).unwrap();
    let mut fixture_processes = Vec::new();
    let mut unrelated = real.then(|| {
        FixtureProcess(
            std::process::Command::new("/bin/sleep")
                .arg("120")
                .process_group(0)
                .spawn()
                .unwrap(),
        )
    });
    let mut issuer_args = vec![bundle.clone(), issuer_config.clone()];
    let mut issuer_environment = json!({"PERSONAL_OAUTH_NATIVE_MEMORY_RESOURCE":resource});
    if real {
        let fixture = PathBuf::from(std::env::var("PHILOTIC_REAL_ISSUER_FIXTURE").unwrap());
        let authority = PathBuf::from(std::env::var("PHILOTIC_REAL_ISSUER_BUNDLE").unwrap());
        assert!(
            std::process::Command::new(&node)
                .env_clear()
                .env("CI", "true")
                .env("PATH", "/usr/bin:/bin")
                .args([
                    fixture.as_os_str(),
                    std::ffi::OsStr::new("prepare"),
                    issuer_config.as_os_str()
                ])
                .status()
                .unwrap()
                .success()
        );
        std::fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::copy(authority, &bundle).unwrap();
        std::fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o444)).unwrap();
        issuer_args = vec![bundle.clone(), root.join("authority.json")];
        issuer_environment["MONGODB_URI"] = json!("mongodb://127.0.0.1:27018");
        fixture_processes.push(FixtureProcess(
            std::process::Command::new(&node)
                .env_clear()
                .env("CI", "true")
                .args([
                    fixture.as_os_str(),
                    std::ffi::OsStr::new("serve"),
                    issuer_config.as_os_str(),
                ])
                .process_group(0)
                .spawn()
                .unwrap(),
        ));
    }
    let (mcp_bin, mcp_sha) = artifact(&binaries.join("membrane-mcp"), &root.join("membrane"));
    let (runner_bin, runner_sha) = artifact(&binaries.join("native-memory"), &root.join("runner"));
    let (node_bin, node_sha) = artifact(&node, &root.join("node"));
    let mut workers = vec![
        json!({"guest":membrane,"agent":"fixture-membrane","roles":["membrane"],"command":mcp_bin,"sha256":mcp_sha,
          "args":["--node-id",hotel,"--guest-id",membrane,"--ipc-socket",socket,"--port",mcp_port.to_string(),"--native-memory-config",ingress],"environment":{}}),
        json!({"guest":runner,"agent":"fixture-runner","roles":["native-memory-runner"],"command":runner_bin,"sha256":runner_sha,
          "args":["--node",hotel,"--guest",runner,"--socket",socket,"--policy",policy,"--response-signing-config",signer,"--memgraph-config",graph,"--authority-socket",control],"environment":{}}),
        json!({"guest":"issuer","agent":"fixture-issuer","roles":["issuer"],"command":node_bin,"sha256":node_sha,
          "args":issuer_args,"environment":issuer_environment,
          "artifacts":[{"path":bundle,"sha256":format!("{:x}",Sha256::digest(std::fs::read(&bundle).unwrap()))}]}),
    ];
    if let Some(bind) = &bridge {
        workers[0]["args"].as_array_mut().unwrap().extend([
            json!("--tls-config"),
            json!("/fixture/bridge/backend/tls.json"),
            json!("--bind-address"),
            json!(bind),
        ]);
    }
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
    let db_path = if real {
        root.join("aiua_context.db")
    } else {
        PathBuf::from(":memory:")
    };
    let store = SqliteGraphStorage::open(&db_path).unwrap();
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
    let materializer = if real {
        None
    } else {
        Some(Arc::new(tokio::sync::Mutex::new(
            crate::service::guest_manager::LocalProcessMaterializer::new(":memory:")
                .with_hotel_socket(socket.to_str().unwrap())
                .with_native_authority(Runtime::load(&config, hotel).await.unwrap()),
        )))
    };
    let manager = materializer.as_ref().map(|materializer| {
        Arc::new(crate::service::guest_manager::GuestManager::new(
            hotel,
            domain.clone(),
            Box::new(SharedMaterializer(materializer.clone())),
        ))
    });
    for guest in ["issuer", runner] {
        let worker = workers.iter().find(|w| w["guest"] == guest).unwrap();
        domain
            .upsert_guest(&ansible_mesh_core::storage::GuestRecord {
                hotel_name: hotel.into(),
                guest_id: guest.into(),
                role: guest.into(),
                config_json: worker.to_string(),
                is_active: true,
                active_pid: unrelated.as_ref().map(|p| p.0.id().to_string()),
                last_active_at: None,
            })
            .unwrap();
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let server = if let Some(manager) = &manager {
        let server =
            crate::service::ipc::IpcServer::new(socket.to_str().unwrap(), hotel, tx, domain)
                .with_materialization_requester(manager.clone());
        Some(tokio::spawn(async move { server.run().await }))
    } else {
        let log = std::fs::File::create(root.join("hotel.log")).unwrap();
        fixture_processes.push(FixtureProcess(
            std::process::Command::new(binaries.join("aiua"))
                .env_clear()
                .env("CI", "true")
                .env("PHILOTIC_HOTEL_SOCKET", &socket)
                .env("PHILOTIC_LOG_DIR", root.join("logs"))
                .current_dir(root)
                .args(["--hotel", hotel, "--native-authority-config"])
                .arg(&config)
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .process_group(0)
                .spawn()
                .unwrap(),
        ));
        None
    };
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
    let mut endpoint_value = json!({"endpoint_id":"native-fixture","owner_agent_id":"fixture-owner","port":mcp_port,"path":null,"exposure":if bridge.is_some(){"lan"}else{"local"},"tools":tools,"default_auth":null,"allow_unauthenticated":false,
        "preapproval_rules":[
            {"action_pattern":"native-memory.remember","target":null,"approved_by_turn":"synthetic","approved_at":0,"expires_at":null},
            {"action_pattern":"native-memory.recall","target":null,"approved_by_turn":"synthetic","approved_at":0,"expires_at":null}
        ],"updated_at":0});
    if renewal {
        for rule in endpoint_value["preapproval_rules"].as_array_mut().unwrap() {
            rule["target"] = json!({"kind":"native_memory","runner_id":runner,"target_node":hotel});
            rule["expires_at"] = json!(now - 1);
        }
    }
    let endpoint = serde_json::from_value(endpoint_value).unwrap();
    if let Some(manager) = &manager {
        for guest in ["issuer", runner] {
            assert!(manager.ensure_guest_active(guest).await.unwrap());
        }
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
    let url = if bridge.is_some() {
        "https://native-package-proxy:8443/native-memory/mcp".to_string()
    } else {
        format!("http://127.0.0.1:{mcp_port}/mcp")
    };
    let health = if bridge.is_some() {
        "https://native-package-proxy:8443/bridge-health".to_string()
    } else {
        format!("http://127.0.0.1:{mcp_port}/health")
    };
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if client
                .get(&health)
                .bearer_auth("synthetic-readiness")
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
                && client
                    .get(format!(
                        "{issuer}/{}",
                        if real { "health" } else { "oauth/introspect" }
                    ))
                    .send()
                    .await
                    .is_ok_and(|r| !real || r.status().is_success())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    if let Some(ref fields) = separate {
        // Same client identity in both probes: positive CA must handshake;
        // unrelated HTTPS issuer CA must be rejected. No system roots used.
        let website_cert = fields["websiteCert"].as_str().unwrap();
        let website_key = fields["websiteKey"].as_str().unwrap();
        let new_ca = fields["authorityCa"].as_str().unwrap();
        for (ca_file, expected) in [(Path::new(new_ca), true), (cert.as_path(), false), (Path::new(new_ca), true)] {
            let mut probe = std::process::Command::new("/usr/bin/openssl");
            probe.args(["s_client", "-connect"]).arg(format!("127.0.0.1:{authority_port}"))
                .args(["-verify_return_error", "-verify_ip", "127.0.0.1", "-no-CApath", "-no-CAstore", "-CAfile"])
                .arg(ca_file).arg("-cert").arg(website_cert).arg("-key").arg(website_key);
            let (status, diagnostic) = fixture_openssl(probe, !expected).unwrap();
            assert_eq!(status.success(), expected, "synthetic authority CA handshake");
            if !expected {
                let diagnostic = std::str::from_utf8(&diagnostic).unwrap();
                assert!(diagnostic.contains("verify error:num=20:") || diagnostic.contains("verify error:num=21:"), "wrong CA must produce certificate verification failure");
            }
        }
    }
    if let Some(bind) = &bridge {
        for (path, expected) in [
            ("/bad-name", 500),
            ("/bad-ca", 500),
            ("/no-client", 502),
            ("/wrong-client", 502),
            ("/personal/mcp", 200),
            ("/native-memory/other", 404),
        ] {
            assert_eq!(
                client
                    .get(format!("https://native-package-proxy:8443{path}"))
                    .bearer_auth("synthetic-readiness")
                    .send()
                    .await
                    .unwrap()
                    .status()
                    .as_u16(),
                expected,
                "{path}"
            );
        }
        assert!(
            client
                .get(format!("https://{bind}:19101/health"))
                .send()
                .await
                .is_err(),
            "direct untrusted TLS peer rejected"
        );
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", 19101))
                .await
                .is_err(),
            "explicit bridge bind must not also bind loopback"
        );
    }
    if let Some(process) = &mut unrelated {
        assert!(
            process.0.try_wait().unwrap().is_none(),
            "stale protected PID must not signal unrelated process"
        );
    }
    let denied_id = uuid::Uuid::new_v4().to_string();
    let mut readonly_token = None;
    if real {
        let issued: Value = client
            .post(format!("{issuer}/test/issue"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        token = issued["token"]
            .as_str()
            .expect("actual PKCE/consent token issuance")
            .into();
        let readonly: Value = client
            .post(format!("{issuer}/test/issue"))
            .header("x-read-only", "1")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        readonly_token = Some(readonly["token"].as_str().unwrap().to_owned());
    }
    let event = uuid::Uuid::new_v4().to_string();
    let remember = json!({"operation":"remember","id":event,"subject":subject,"content":"Synthetic violet lantern","source":"synthetic://oauth","source_version":"1","lineage":"independent"});
    let mut renewal_denied_ids = Vec::new();
    let mut renewal_before = None;
    if renewal {
        let snapshot = || live_guest_snapshot(&db_path, hotel, [membrane, runner, "issuer"]);
        let before = snapshot();
        renewal_denied_ids.push(
            denied_renewal_phase(
                &client,
                &url,
                &token,
                subject,
                -32602,
                "native memory requires explicit bearer auth and preapproval",
            )
            .await,
        );
        let IpcResponse::Standard {
            ok: true,
            data: Some(status),
            ..
        } = probe
            .send_request(IpcRequest::GetMcpEndpointStatus {
                endpoint_id: "native-fixture".into(),
            })
            .await
            .unwrap()
        else {
            panic!("packaged endpoint status unavailable")
        };
        let expiry = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 600;
        let old_digest = status["config_sha256"].as_str().unwrap().to_owned();
        let update = |digest: String| IpcRequest::ExtendNativeMcpPreapprovals {
            endpoint_id: "native-fixture".into(),
            expected_config_sha256: digest,
            expires_at: expiry,
            approval_reference: "synthetic exact-package renewal".into(),
        };
        let IpcResponse::Standard {
            ok: true,
            data: Some(updated),
            ..
        } = probe
            .send_request(update(old_digest.clone()))
            .await
            .unwrap()
        else {
            panic!("packaged maintenance failed")
        };
        assert_eq!(updated["persisted"], true);
        assert_eq!(updated["push_enqueued"], true);
        assert_eq!(updated["lifecycle_changed"], false);
        assert_eq!(updated["effective_state_verified"], false);
        assert!(matches!(
            probe.send_request(update(old_digest)).await.unwrap(),
            IpcResponse::Standard { ok: false, .. }
        ));
        // Reconciliation repeats only the push, without touching lifecycle state.
        for _ in 0..3 {
            assert!(matches!(
                probe
                    .send_request(update(updated["config_sha256"].as_str().unwrap().into()))
                    .await
                    .unwrap(),
                IpcResponse::Standard { ok: true, .. }
            ));
            tokio::time::sleep(Duration::from_secs(2)).await;
            assert_eq!(
                snapshot(),
                before,
                "supervision must preserve every running generation"
            );
        }
        // membrane-mcp protocol::error_code::DISPATCH_ERROR is -32004.
        renewal_denied_ids
            .push(denied_renewal_phase(&client, &url, &token, subject, -32004, "Denied").await);
        let renew_file = |path: &Path| {
            let mut value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            value["grants"]["fixture"]["revision"] = json!(2);
            value["grants"]["fixture"]["expires_at"] = json!(expiry);
            let temporary = path.with_extension("renewal");
            json_file(&temporary, value);
            std::fs::rename(temporary, path).unwrap();
        };
        renew_file(&ingress);
        // A valid new ingress grant cannot override the still-expired receiver policy.
        renewal_denied_ids.push(
            denied_renewal_phase(
                &client,
                &url,
                &token,
                subject,
                -32001,
                "native OAuth denied",
            )
            .await,
        );
        renew_file(&policy);
        assert_eq!(snapshot(), before);
        renewal_before = Some(before);
        println!(
            "qualification: expired grants denied; approval-only renewal denied; new ingress/stale receiver denied; PID/starttime/config unchanged across supervision"
        );
    }

    // Scope denial is tested only after endpoint/grant expiry gates have cleared.
    if let Some(readonly_token) = readonly_token {
        let denied = rpc(
            &client,
            &url,
            readonly_token.as_str(),
            "native-memory.remember",
            &denied_id,
            json!({"operation":"remember","id":denied_id,"subject":subject,"content":"Synthetic forbidden violet lantern","source":"synthetic://readonly","source_version":"1","lineage":"independent"}),
        )
        .await;
        assert_eq!(denied["error"]["code"], -32001);
    }
    if bridge.is_some() {
        let response = client.post(&url).json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"native-memory.remember","arguments":remember}})).send().await.unwrap();
        if response.status().as_u16() != 401 {
            let denied: Value = response.json().await.unwrap();
            assert_eq!(denied["error"]["code"], -32001);
        }
    }
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
        !recalled["observations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["id"] == denied_id)
    );
    for denied in &renewal_denied_ids {
        assert!(
            !recalled["observations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o["id"] == *denied),
            "denied renewal phase must not commit an observation"
        );
    }
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
    if let Some(before) = renewal_before {
        assert_eq!(
            live_guest_snapshot(&db_path, hotel, [membrane, runner, "issuer"]),
            before,
            "capture/recall/revocation must preserve supervised guest generations"
        );
        println!(
            "qualification: synchronized renewal captured/recalled; all denied IDs absent; revocation denied; final guest generations unchanged"
        );
    }
    if let Some(materializer) = materializer {
        for guest in [membrane, runner, "issuer"] {
            materializer
                .lock()
                .await
                .reclaim_guest(guest)
                .await
                .unwrap();
        }
    }
    if let Some(server) = server {
        server.abort();
    }
    drain.abort();
}
