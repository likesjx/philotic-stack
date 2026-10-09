//! Synthetic, source-only protected text acceptance. No stack services, vaults,
//! credentials, network provider or production authority installation.
//! The parent owns a real child, the canonical issuer and exact payload catalog;
//! the child runs the existing SDK and recall assembler over framed Unix IPC.
use agent_core::{
    recall_authority::{CanonicalRecallCatalog, RecallRpcRequest},
    recall_selection::RecallAuthority,
    session::{RecalledMemoryRecord, SessionState, ToolDefinition},
};
use ansible_mesh_core::{
    privacy::{
        AuthenticatedAgent, ProcessingOperation, ProviderBoundary, ResourcePolicy,
        ServerAuthenticatedIdentity,
    },
    privacy_local::{
        LaunchPrincipal, LocalLaunchRegistry, LocalManifestAuthority, LocalTaskAuthority,
        LocalTaskEnvelope,
    },
    privacy_rpc::LocalAuthorityRpc,
    privacy_storage::PolicyStore,
};
use philotic_client::{
    GuestIdentity, IpcRequest, IpcResponse, PhiloticClient, protected_authority::TrustedHotelPeer,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
};
use uuid::Uuid;

const AGENT: &str = "synthetic-agent";
const HUMAN: &str = "synthetic-human";
const SESSION: &str = "synthetic-session";
const HOTEL: &str = "synthetic-hotel";
const GUEST: &str = "synthetic-consumer";
const FACT: &str = "CANONICAL_RECALL_MARKER";
const TOOL_RESULT: &str = "COMPLETE_TOOL_RESULT_MARKER";
const TIMEOUT: Duration = Duration::from_secs(5);

struct Owner;
impl ServerAuthenticatedIdentity for Owner {
    fn stable_agent_id(&self) -> &str {
        "synthetic-owner"
    }
    fn roles(&self) -> BTreeSet<String> {
        BTreeSet::new()
    }
}
fn owner() -> AuthenticatedAgent {
    AuthenticatedAgent::from_server(&Owner).unwrap()
}
fn record() -> RecalledMemoryRecord {
    RecalledMemoryRecord {
        id: Some("synthetic-record".into()),
        vault_id: Some("synthetic-vault".into()),
        concept: "fixture".into(),
        content: FACT.into(),
        source: Some("CANONICAL_PROVENANCE".into()),
        annotations: Some(json!({"owner_revision":1})),
        ..Default::default()
    }
}
fn tools() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        tool_name: "fixture.echo".into(),
        description: "SYNTHETIC_TOOL_SCHEMA".into(),
        input_schema: json!({"type":"object","properties":{"input":{"type":"string"}}}),
        ..Default::default()
    }]
}
fn input_payload() -> String {
    serde_json::to_string(&json!({"actor":AGENT,"human_principal":HUMAN,
        "session":SESSION,"user_content":"SYNTHETIC_USER","records":[record()],
        "tools":tools(),"tool_results":[{"tool_name":"fixture.echo",
        "arguments":{"input":"COMPLETE_TOOL_ARGUMENT"},"result":TOOL_RESULT}],
        "instructions":"synthetic fixture operational instructions"}))
    .unwrap()
}
fn state(case: &str) -> SessionState {
    let initial = SessionState::new(SESSION.into(), AGENT.into(), "synthetic-channel".into());
    let mut checkpoint = initial.checkpoint_json();
    checkpoint["active_turn"] = json!({"turn_id":"synthetic-turn","phase":"waiting_tool","user_content":"SYNTHETIC_USER",
        "recalled_memories":[record()], "working_tool_history":[[
        {"tool_name":"fixture.echo","arguments":{"input":"COMPLETE_TOOL_ARGUMENT"}},
        {"tool_name":"fixture.echo","content":TOOL_RESULT}]]});
    let mut state = SessionState::from_checkpoint(&checkpoint).unwrap();
    state.agent_profile.user_principal_id = Some(HUMAN.into());
    match case {
        "record-content" => state.active_turn.as_mut().unwrap().recalled_memories[0]
            .content
            .push_str(" forged"),
        "record-provenance" => {
            state.active_turn.as_mut().unwrap().recalled_memories[0].source =
                Some("forged provenance".into())
        }
        "record-metadata" => {
            state.active_turn.as_mut().unwrap().recalled_memories[0].annotations =
                Some(json!({"owner_revision":99}))
        }
        "principal" => state.agent_profile.user_principal_id = Some("forged-human".into()),
        "session" => state.session_id = "forged-session".into(),
        "agent" => state.agent_id = "forged-agent".into(),
        _ => {}
    }
    state
}
fn binding_matches(
    principal: &str,
    agent: &str,
    session: &str,
    candidate: &RecalledMemoryRecord,
) -> bool {
    principal == HUMAN && agent == AGENT && session == SESSION && candidate == &record()
}
// Parent-owned canonical fixture data, never populated from the child's payload.
struct Catalog {
    gate: Option<PathBuf>,
}
impl CanonicalRecallCatalog for Catalog {
    fn resource_for_record(
        &self,
        actor: &AuthenticatedAgent,
        principal: &str,
        session: &str,
        candidate: &RecalledMemoryRecord,
    ) -> Option<String> {
        if !binding_matches(principal, actor.stable_agent_id(), session, candidate) {
            return None;
        }
        if let Some(dir) = &self.gate {
            std::fs::write(dir.join("entered"), b"synthetic gate").unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            while !dir.join("release").exists() {
                assert!(Instant::now() < deadline, "assembly gate timed out");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Some("memory-source".into())
    }
}
struct ExpectedRecall;
impl RecallAuthority for ExpectedRecall {
    fn permits(
        &self,
        principal: &str,
        agent: &str,
        session: &str,
        candidate: &RecalledMemoryRecord,
    ) -> bool {
        binding_matches(principal, agent, session, candidate)
    }
}
fn wire((prompt, mut context, context_projection): (String, Value, Value)) -> String {
    // Only the fake renderer freezes the runtime clock so the parent can
    // independently derive exact expected bytes. No production clock changes.
    let clocks = context["instructions"].as_array_mut().unwrap();
    let clock = clocks
        .iter_mut()
        .find(|item| item["projection_kind"] == "clock")
        .unwrap();
    clock["text"] = json!("Current date and time (UTC): 2026-01-01 00:00:00 UTC\n");
    serde_json::to_string(&json!({"kind":"text.generate","model":"synthetic-model","prompt":prompt,"context":context,"context_projection":context_projection,"tools":tools()})).unwrap()
}
fn expected_wire(case: &str) -> String {
    wire(state(case).model_request_payloads_with_recall_authority(
        "SYNTHETIC_USER",
        &tools(),
        Some(&ExpectedRecall),
    ))
}
struct Manifest {
    input: String,
    output: String,
}
impl LocalManifestAuthority for Manifest {
    fn sources(&self, _: Uuid, payload: &str, actor: &AuthenticatedAgent) -> Option<Vec<String>> {
        if actor.stable_agent_id() != AGENT || (payload != self.input && payload != self.output) {
            return None;
        }
        let mut sources = vec!["instruction-source".into(), "tool-source".into()];
        if payload == self.input || payload.contains(FACT) {
            sources.push("memory-source".into());
        }
        Some(sources)
    }
}
async fn config(client: &mut PhiloticClient, key: String) -> LocalTaskEnvelope {
    match client
        .send_request_with_timeout(IpcRequest::GetConfig { key }, TIMEOUT)
        .await
        .unwrap()
    {
        IpcResponse::ConfigData {
            value_json: Some(data),
            ..
        } => serde_json::from_str(&data).unwrap(),
        other => panic!("wrong synthetic fixture reply: {other:?}"),
    }
}
// Fixture-only provider: its endpoint is the actual endpoint used at invocation,
// not a caller-supplied boundary hint. No real provider registry is installed.
async fn fake_text_attempt(
    client: &mut PhiloticClient,
    peer: TrustedHotelPeer,
    envelope: &LocalTaskEnvelope,
    payload: &str,
    endpoint: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        envelope.payload == payload,
        "assembled payload binding mismatch"
    );
    client
        .resolve_local_authority(
            peer,
            envelope,
            endpoint,
            ProcessingOperation::Inference,
            TIMEOUT,
        )
        .await?;
    // Only a successful fresh resolution reaches the fake provider.
    client
        .send_request_with_timeout(
            IpcRequest::GetConfig {
                key: format!("invoked:{endpoint}"),
            },
            TIMEOUT,
        )
        .await?;
    Ok(())
}

#[test]
#[ignore = "launched only by the synthetic parent fixture"]
fn supervised_protected_text_child() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let dir = PathBuf::from(std::env::var("CONTEXT_ACCEPTANCE_DIR").unwrap());
        let case = std::env::var("CONTEXT_ACCEPTANCE_CASE").unwrap();
        let peer = TrustedHotelPeer {
            pid: std::env::var("CONTEXT_ACCEPTANCE_PARENT_PID")
                .unwrap()
                .parse()
                .unwrap(),
            uid: std::env::var("CONTEXT_ACCEPTANCE_UID")
                .unwrap()
                .parse()
                .unwrap(),
        };
        let mut client = PhiloticClient::connect_at(
            dir.join("hotel.sock").to_str().unwrap(),
            GuestIdentity {
                guest_id: GUEST.into(),
                role: "untrusted-administrator-claim".into(),
                supported_tools: vec![],
            },
        )
        .await
        .unwrap();
        let envelope = config(&mut client, "inbound".into()).await;
        let state = state(&case);
        let original = state
            .active_turn
            .as_ref()
            .unwrap()
            .recalled_memories
            .clone();
        let original_tools = state
            .active_turn
            .as_ref()
            .unwrap()
            .working_tool_history
            .clone();
        assert!(
            !wire(state.model_request_payloads("SYNTHETIC_USER", &tools())).contains(FACT),
            "legacy assembly must keep recall closed"
        );
        let gate =
            matches!(case.as_str(), "worker-revoke" | "worker-cancel").then_some(dir.clone());
        let result = state
            .model_request_payloads_with_rpc_recall(
                "SYNTHETIC_USER",
                &tools(),
                &mut client,
                RecallRpcRequest {
                    hotel: peer,
                    envelope: &envelope,
                    endpoint: "local",
                    timeout: TIMEOUT,
                },
                Arc::new(Catalog { gate }),
            )
            .await;
        if matches!(
            case.as_str(),
            "pre-revoke" | "pre-cancel" | "worker-revoke" | "worker-cancel" | "rpc-failure"
        ) {
            assert!(
                result.is_err(),
                "denied assembly must return an error, never legacy fallback: {case}"
            );
            assert_eq!(
                state.active_turn.as_ref().unwrap().recalled_memories,
                original
            );
            assert_eq!(
                state.active_turn.as_ref().unwrap().working_tool_history,
                original_tools
            );
            return;
        }
        let mut payload = wire(result.unwrap());
        assert!(payload.contains(TOOL_RESULT));
        assert!(payload.contains("COMPLETE_TOOL_ARGUMENT"));
        assert!(payload.contains("SYNTHETIC_TOOL_SCHEMA"));
        assert_eq!(
            payload.contains(FACT),
            !matches!(
                case.as_str(),
                "record-content"
                    | "record-provenance"
                    | "record-metadata"
                    | "principal"
                    | "session"
                    | "agent"
            )
        );
        assert_eq!(
            state.active_turn.as_ref().unwrap().recalled_memories,
            original
        );
        assert_eq!(
            state.active_turn.as_ref().unwrap().working_tool_history,
            original_tools
        );
        let mut outgoing = config(&mut client, format!("bind:{payload}")).await;
        if case == "payload" {
            payload.push(' ');
        }
        if case == "envelope" {
            outgoing.payload.push(' ');
            payload = outgoing.payload.clone();
        }
        let endpoint = if case == "endpoint" {
            "local-cloud-proxy"
        } else {
            "local"
        };
        let result = fake_text_attempt(&mut client, peer, &outgoing, &payload, endpoint).await;
        assert_eq!(
            result.is_err(),
            matches!(
                case.as_str(),
                "payload" | "envelope" | "endpoint" | "dispatch-revoke"
            )
        );
    });
}

struct Cleanup {
    dir: PathBuf,
    child: Arc<Mutex<std::process::Child>>,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
async fn read_frame(stream: &mut UnixStream) -> Option<IpcRequest> {
    let n = match stream.read_u32().await {
        Ok(n) => n,
        Err(_) => return None,
    };
    assert!(n <= 1_048_576);
    let mut bytes = vec![0; n as usize];
    stream.read_exact(&mut bytes).await.unwrap();
    Some(serde_json::from_slice(&bytes).unwrap())
}
async fn reply(stream: &mut UnixStream, response: IpcResponse) {
    let bytes = serde_json::to_vec(&response).unwrap();
    stream.write_u32(bytes.len() as u32).await.unwrap();
    stream.write_all(&bytes).await.unwrap();
}
async fn run_case(case: &str) {
    let dir = std::env::temp_dir().join(format!("context-accept-{}", Uuid::new_v4().simple()));
    std::fs::create_dir(&dir).unwrap();
    let listener = UnixListener::bind(dir.join("hotel.sock")).unwrap();
    // UID comes from an actual local socket peer, not a task identity claim.
    let (uid_probe, _) = UnixStream::pair().unwrap();
    let uid = uid_probe.peer_cred().unwrap().uid();
    let log = std::fs::File::create(dir.join("child.log")).unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "supervised_protected_text_child",
            "--nocapture",
        ])
        .env("CONTEXT_ACCEPTANCE_DIR", &dir)
        .env("CONTEXT_ACCEPTANCE_CASE", case)
        .env(
            "CONTEXT_ACCEPTANCE_PARENT_PID",
            std::process::id().to_string(),
        )
        .env("CONTEXT_ACCEPTANCE_UID", uid.to_string())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    let child = Arc::new(Mutex::new(child));
    let cleanup = Cleanup {
        dir: dir.clone(),
        child: child.clone(),
    };
    let launches = Arc::new(LocalLaunchRegistry::default());
    launches
        .attach(
            GUEST,
            uid,
            LaunchPrincipal {
                stable_agent_id: AGENT.into(),
                roles: BTreeSet::new(),
            },
            child.clone(),
        )
        .unwrap();
    let (mut stream, _) = tokio::time::timeout(TIMEOUT, listener.accept())
        .await
        .unwrap()
        .unwrap();
    let session = launches.authenticate(HOTEL, &stream, GUEST).unwrap();
    let store = Arc::new(PolicyStore::open(dir.join("synthetic.db")).unwrap());
    for source in ["instruction-source", "memory-source", "tool-source"] {
        store
            .insert_policy(
                &owner(),
                source,
                &ResourcePolicy::private("synthetic-owner".into(), AGENT.into()),
            )
            .unwrap();
    }
    let expected = expected_wire(case);
    let authority = Arc::new(
        LocalTaskAuthority::new(
            HOTEL.into(),
            launches.clone(),
            store.clone(),
            Arc::new(Manifest {
                input: input_payload(),
                output: expected.clone(),
            }),
            16,
        )
        .unwrap(),
    );
    let inbound = authority
        .issue(
            session.clone(),
            session.consumer(),
            Uuid::new_v4(),
            input_payload(),
            Duration::from_secs(60),
        )
        .unwrap();
    let rpc = Arc::new(
        LocalAuthorityRpc::new(
            HOTEL.into(),
            launches,
            authority.clone(),
            BTreeMap::from([
                ("local".into(), ProviderBoundary::LocalTrusted),
                ("local-cloud-proxy".into(), ProviderBoundary::External),
            ]),
        )
        .unwrap(),
    );
    let mutation = if matches!(case, "worker-revoke" | "worker-cancel") {
        let (dir, authority, session, inbound, store) = (
            dir.clone(),
            authority.clone(),
            session.clone(),
            inbound.clone(),
            store.clone(),
        );
        let cancel = case == "worker-cancel";
        Some(tokio::spawn(async move {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !dir.join("entered").exists() {
                assert!(
                    Instant::now() < deadline,
                    "worker did not reach canonical lookup"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            tokio::task::spawn_blocking(move || {
                if cancel {
                    authority.cancel_envelope(&session, &inbound).unwrap();
                } else {
                    store.revoke_creator(&owner(), "memory-source").unwrap();
                }
                std::fs::write(dir.join("release"), b"synthetic release").unwrap();
            })
            .await
            .unwrap();
        }))
    } else {
        None
    };
    let mut calls = 0;
    let mut resolutions = 0;
    while let Some(request) = read_frame(&mut stream).await {
        let response = match request {
            IpcRequest::Register(identity) => {
                assert_eq!(identity.guest_id, GUEST);
                assert_eq!(session.principal().unwrap().stable_agent_id(), AGENT);
                IpcResponse::success("synthetic register ack is not authority", None)
            }
            IpcRequest::GetConfig { key } if key == "inbound" => {
                if case == "pre-revoke" {
                    store.revoke_creator(&owner(), "memory-source").unwrap();
                }
                if case == "pre-cancel" {
                    authority.cancel_envelope(&session, &inbound).unwrap();
                }
                IpcResponse::ConfigData {
                    key,
                    value_json: Some(serde_json::to_string(&inbound).unwrap()),
                }
            }
            IpcRequest::GetConfig { key } if key.starts_with("bind:") => {
                assert_eq!(
                    &key[5..],
                    expected,
                    "owner rejects incomplete or modified assembled payload"
                );
                let outgoing = authority
                    .issue(
                        session.clone(),
                        session.consumer(),
                        Uuid::new_v4(),
                        expected.clone(),
                        Duration::from_secs(30),
                    )
                    .unwrap();
                if case == "dispatch-revoke" {
                    store.revoke_creator(&owner(), "tool-source").unwrap();
                }
                IpcResponse::ConfigData {
                    key,
                    value_json: Some(serde_json::to_string(&outgoing).unwrap()),
                }
            }
            IpcRequest::GetConfig { key } if key.starts_with("invoked:") => {
                assert_eq!(key, "invoked:local");
                calls += 1;
                IpcResponse::success("synthetic provider invoked", None)
            }
            IpcRequest::ProtectedAuthority(request) => {
                resolutions += 1;
                if case == "rpc-failure" {
                    IpcResponse::success("wrong generic ack", None)
                } else {
                    let authenticated = rpc.authenticate(&stream, &request).unwrap();
                    let rpc = rpc.clone();
                    let protected_authority =
                        tokio::task::spawn_blocking(move || rpc.handle(&authenticated, &request))
                            .await
                            .unwrap();
                    IpcResponse::ProtectedAuthorityReply {
                        protected_authority,
                    }
                }
            }
            other => panic!("unexpected fixture request: {other:?}"),
        };
        reply(&mut stream, response).await;
    }
    if let Some(mutation) = mutation {
        mutation.await.unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.lock().unwrap().try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "child did not exit");
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert!(
        status.success(),
        "case {case}: {}",
        std::fs::read_to_string(dir.join("child.log")).unwrap()
    );
    let denied = matches!(
        case,
        "pre-revoke"
            | "pre-cancel"
            | "worker-revoke"
            | "worker-cancel"
            | "rpc-failure"
            | "payload"
            | "envelope"
            | "endpoint"
            | "dispatch-revoke"
    );
    assert_eq!(
        calls,
        usize::from(!denied),
        "provider invocation count: {case}"
    );
    assert!(resolutions > 0, "must exercise real RPC: {case}");
    drop(cleanup);
}
async fn bounded_case(case: &str) {
    tokio::time::timeout(Duration::from_secs(30), run_case(case))
        .await
        .expect("synthetic acceptance timed out");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canonical_record_subject_and_tool_result_acceptance() {
    for case in [
        "happy",
        "record-content",
        "record-provenance",
        "record-metadata",
        "principal",
        "session",
        "agent",
    ] {
        bounded_case(case).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pre_and_post_assembly_revocation_cancellation_and_rpc_failure_deny_without_fallback() {
    for case in [
        "pre-revoke",
        "pre-cancel",
        "worker-revoke",
        "worker-cancel",
        "rpc-failure",
    ] {
        bounded_case(case).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_outgoing_payload_actual_endpoint_and_fresh_policy_guard_fake_provider() {
    for case in ["payload", "envelope", "endpoint", "dispatch-revoke"] {
        bounded_case(case).await;
    }
}
