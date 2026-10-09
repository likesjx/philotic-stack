use ansible_mesh_core::privacy::*;
use ansible_mesh_core::privacy_local::*;
use ansible_mesh_core::privacy_storage::*;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use uuid::Uuid;

// Runs only as a synthetic child, never as a stack service. It deliberately
// claims administrator in JSON; the kernel/owned-child binding ignores that.
#[test]
#[ignore]
fn supervised_fixture_child() {
    use std::io::{Read, Write};
    let path = std::env::var("SYNTHETIC_PRIVACY_SOCKET").unwrap();
    let mut stream = std::os::unix::net::UnixStream::connect(path).unwrap();
    stream
        .write_all(b"{\"guest_id\":\"victim\",\"role\":\"administrator\"}\n")
        .unwrap();
    let mut byte = [0];
    let _ = stream.read(&mut byte);
}
struct Owner;
impl ServerAuthenticatedIdentity for Owner {
    fn stable_agent_id(&self) -> &str {
        "owner"
    }
    fn roles(&self) -> BTreeSet<String> {
        BTreeSet::new()
    }
}
struct Manifest;
impl LocalManifestAuthority for Manifest {
    fn sources(&self, _: Uuid, payload: &str, _: &AuthenticatedAgent) -> Option<Vec<String>> {
        // Synthetic provenance catalog recognises this complete payload only.
        (payload == "synthetic complete payload").then(|| vec!["source".into()])
    }
}
struct Fixture {
    dir: tempfile::TempDir,
    registry: Arc<LocalLaunchRegistry>,
    store: Arc<PolicyStore>,
    streams: Vec<UnixStream>,
    children: Vec<Arc<Mutex<std::process::Child>>>,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(PolicyStore::open(dir.path().join("synthetic.db")).unwrap());
        store
            .insert_policy(
                &AuthenticatedAgent::from_server(&Owner).unwrap(),
                "source",
                &ResourcePolicy::private("owner".into(), "agent:synthetic".into()),
            )
            .unwrap();
        Self {
            dir,
            registry: Arc::new(LocalLaunchRegistry::default()),
            store,
            streams: vec![],
            children: vec![],
        }
    }
    async fn child(&mut self, guest: &str, agent: &str) -> Arc<VerifiedLocalSession> {
        let path = self.dir.path().join(format!("{}.sock", Uuid::new_v4()));
        let listener = UnixListener::bind(&path).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "supervised_fixture_child",
                "--nocapture",
            ])
            .env("SYNTHETIC_PRIVACY_SOCKET", &path)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let child = Arc::new(Mutex::new(child));
        let (stream, _) = listener.accept().await.unwrap();
        let uid = stream.peer_cred().unwrap().uid();
        self.registry
            .attach(
                guest,
                uid,
                LaunchPrincipal {
                    stable_agent_id: agent.into(),
                    roles: BTreeSet::from(["worker".into()]),
                },
                child.clone(),
            )
            .unwrap();
        let session = self
            .registry
            .authenticate("synthetic-hotel", &stream, guest)
            .unwrap();
        self.children.push(child);
        self.streams.push(stream);
        session
    }
    fn authority(&self, limit: usize) -> LocalTaskAuthority {
        LocalTaskAuthority::new(
            "synthetic-hotel".into(),
            self.registry.clone(),
            self.store.clone(),
            Arc::new(Manifest),
            limit,
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for c in &self.children {
            let mut c = c.lock().unwrap();
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}
#[tokio::test]
async fn kernel_child_identity_rejects_guest_spoof_and_caller_admin_role() {
    let mut f = Fixture::new();
    let origin = f.child("origin", "agent:synthetic").await;
    let _consumer = f.child("consumer", "model:synthetic").await;
    assert!(f
        .registry
        .authenticate("synthetic-hotel", &f.streams[0], "consumer")
        .is_err());
    let actor = origin.principal().unwrap();
    assert_eq!(actor.stable_agent_id(), "agent:synthetic");
    let mut role_policy = ResourcePolicy::private("owner".into(), "other".into());
    role_policy.read_roles.insert("administrator".into());
    f.store
        .insert_policy(
            &AuthenticatedAgent::from_server(&Owner).unwrap(),
            "admin-only",
            &role_policy,
        )
        .unwrap();
    assert!(authorize_read(&f.store.snapshot().unwrap(), Some(&actor), "admin-only").is_err());
}
#[tokio::test]
async fn protected_delivery_replay_and_park_bind_full_payload_and_exact_consumer() {
    let mut f = Fixture::new();
    let origin = f.child("origin", "agent:synthetic").await;
    let consumer = f.child("consumer", "model:synthetic").await;
    let wrong = f.child("wrong", "model:other").await;
    let a = f.authority(8);
    let task = Uuid::new_v4();
    let e = a
        .issue(
            origin.clone(),
            consumer.consumer(),
            task,
            "synthetic complete payload".into(),
            Duration::from_secs(60),
        )
        .unwrap();
    assert_eq!(
        a.issue(
            origin,
            consumer.consumer(),
            task,
            e.payload.clone(),
            Duration::from_secs(60)
        )
        .unwrap(),
        e
    );
    let parked = a.park(&e).unwrap();
    let replay: ParkedLocalTask =
        serde_json::from_str(&serde_json::to_string(&parked).unwrap()).unwrap();
    assert_eq!(a.flush(&replay, &consumer).unwrap(), e);
    let resolved = a
        .resolve(
            &e,
            &consumer,
            ProcessingOperation::Inference,
            ProviderBoundary::LocalTrusted,
        )
        .unwrap();
    assert_eq!(resolved.actor.stable_agent_id(), "agent:synthetic");
    assert_eq!(resolved.sources, vec!["source"]);
    assert!(a
        .resolve(
            &e,
            &wrong,
            ProcessingOperation::Inference,
            ProviderBoundary::LocalTrusted
        )
        .is_err());
    assert!(a
        .resolve(
            &e,
            &consumer,
            ProcessingOperation::TextToSpeech,
            ProviderBoundary::External
        )
        .is_err());
    let mut forged = e.clone();
    forged.payload.push_str(" changed tools/summary");
    assert!(a
        .flush(&ParkedLocalTask { envelope: forged }, &consumer)
        .is_err());
    let mut forged = e.clone();
    forged.task_id = Uuid::new_v4();
    assert!(a
        .resolve(
            &forged,
            &consumer,
            ProcessingOperation::Inference,
            ProviderBoundary::LocalTrusted
        )
        .is_err());
    let mut forged = e;
    forged.authority_handle = Uuid::new_v4();
    assert!(a
        .resolve(
            &forged,
            &consumer,
            ProcessingOperation::Inference,
            ProviderBoundary::LocalTrusted
        )
        .is_err());
}
#[tokio::test]
async fn creator_revocation_blocks_parked_delivery_resolution_and_graph_pin() {
    let mut f = Fixture::new();
    let origin = f.child("origin", "agent:synthetic").await;
    let consumer = f.child("consumer", "model:synthetic").await;
    let a = f.authority(8);
    let e = a
        .issue(
            origin,
            consumer.consumer(),
            Uuid::new_v4(),
            "synthetic complete payload".into(),
            Duration::from_secs(60),
        )
        .unwrap();
    let parked = a.park(&e).unwrap();
    let (_, lease) = a.pin_capture(&e, &consumer).unwrap();
    drop(lease);
    f.store
        .revoke_creator(&AuthenticatedAgent::from_server(&Owner).unwrap(), "source")
        .unwrap();
    assert!(a.flush(&parked, &consumer).is_err());
    assert!(a.pin_capture(&e, &consumer).is_err());
    assert!(a
        .resolve(
            &e,
            &consumer,
            ProcessingOperation::SpeechToText,
            ProviderBoundary::LocalTrusted
        )
        .is_err());
}
#[tokio::test]
async fn cancelled_task_cannot_reissue_repark_or_resolve_and_wrong_actor_cannot_cancel() {
    let mut f = Fixture::new();
    let origin = f.child("origin", "agent:synthetic").await;
    let consumer = f.child("consumer", "model:synthetic").await;
    let a = f.authority(1);
    let e = a
        .issue(
            origin.clone(),
            consumer.consumer(),
            Uuid::new_v4(),
            "synthetic complete payload".into(),
            Duration::from_secs(60),
        )
        .unwrap();
    assert!(a.cancel(&consumer, e.authority_handle).is_err());
    a.cancel(&origin, e.authority_handle).unwrap();
    a.cancel(&origin, e.authority_handle).unwrap();
    assert!(a
        .resolve(
            &e,
            &consumer,
            ProcessingOperation::Inference,
            ProviderBoundary::LocalTrusted
        )
        .is_err());
    assert!(a.park(&e).is_err());
    assert!(a
        .issue(
            origin.clone(),
            consumer.consumer(),
            e.task_id,
            e.payload.clone(),
            Duration::from_secs(60)
        )
        .is_err());
    assert!(a
        .issue(
            origin,
            consumer.consumer(),
            Uuid::new_v4(),
            e.payload,
            Duration::from_secs(60)
        )
        .is_err());
}
#[tokio::test]
async fn consumer_replacement_invalidates_old_session_and_parked_binding() {
    let mut f = Fixture::new();
    let origin = f.child("origin", "agent:synthetic").await;
    let consumer = f.child("consumer", "model:synthetic").await;
    let a = f.authority(8);
    let e = a
        .issue(
            origin,
            consumer.consumer(),
            Uuid::new_v4(),
            "synthetic complete payload".into(),
            Duration::from_secs(60),
        )
        .unwrap();
    let parked = a.park(&e).unwrap();
    let replacement = f.child("consumer", "model:synthetic").await;
    assert!(consumer.principal().is_err());
    assert!(a.flush(&parked, &replacement).is_err());
    assert!(a.flush(&parked, &consumer).is_err());
}
#[tokio::test]
async fn exited_owned_child_and_unknown_manifest_fail_closed() {
    let mut f = Fixture::new();
    let origin = f.child("origin", "agent:synthetic").await;
    let consumer = f.child("consumer", "model:synthetic").await;
    let a = f.authority(8);
    assert!(a
        .issue(
            origin.clone(),
            consumer.consumer(),
            Uuid::new_v4(),
            "caller claimed source/roles".into(),
            Duration::from_secs(60)
        )
        .is_err());
    {
        let mut child = f.children[0].lock().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
    }
    assert!(origin.principal().is_err());
    assert!(a
        .issue(
            origin,
            consumer.consumer(),
            Uuid::new_v4(),
            "synthetic complete payload".into(),
            Duration::from_secs(60)
        )
        .is_err());
}

#[tokio::test]
async fn protected_cross_hotel_and_expired_authority_are_denied() {
    let mut f = Fixture::new();
    let origin = f.child("origin", "agent:synthetic").await;
    let consumer = f.child("consumer", "model:synthetic").await;
    let authority = f.authority(8);
    let foreign = f.registry.target("other-hotel", "consumer").unwrap();
    assert!(authority
        .issue(
            origin.clone(),
            foreign,
            Uuid::new_v4(),
            "synthetic complete payload".into(),
            Duration::from_secs(60)
        )
        .is_err());
    let envelope = authority
        .issue(
            origin,
            consumer.consumer(),
            Uuid::new_v4(),
            "synthetic complete payload".into(),
            Duration::from_millis(1),
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(5));
    assert!(authority
        .resolve(
            &envelope,
            &consumer,
            ProcessingOperation::Inference,
            ProviderBoundary::LocalTrusted
        )
        .is_err());
    assert!(authority.park(&envelope).is_err());
}

#[tokio::test]
async fn protected_rpc_authenticates_kernel_peer_and_binds_resolve_cancel() {
    use ansible_mesh_core::privacy_rpc::*;
    use std::collections::BTreeMap;
    let mut f = Fixture::new();
    let origin = f.child("origin-rpc", "agent:synthetic").await;
    let consumer = f.child("consumer-rpc", "agent:consumer").await;
    let authority = Arc::new(f.authority(32));
    let envelope = authority
        .issue(
            origin.clone(),
            consumer.consumer(),
            Uuid::new_v4(),
            "synthetic complete payload".into(),
            Duration::from_secs(30),
        )
        .unwrap();
    let rpc = LocalAuthorityRpc::new(
        "synthetic-hotel".into(),
        f.registry.clone(),
        authority.clone(),
        BTreeMap::from([
            ("local".into(), ProviderBoundary::LocalTrusted),
            ("cloud".into(), ProviderBoundary::External),
        ]),
    )
    .unwrap();
    let request = ProtectedAuthorityRequest::Resolve {
        request_id: Uuid::new_v4(),
        guest: "consumer-rpc".into(),
        envelope: envelope.clone(),
        endpoint: "local".into(),
        operation: ProcessingOperation::Inference,
    };
    let proof = rpc.authenticate(&f.streams[1], &request).unwrap();
    assert!(rpc.authenticate(&f.streams[0], &request).is_err());
    let mut spoofed = request.clone();
    if let ProtectedAuthorityRequest::Resolve { guest, .. } = &mut spoofed {
        *guest = "victim".into();
    }
    assert!(matches!(
        rpc.handle(&proof, &spoofed).outcome,
        ProtectedAuthorityOutcome::Denied
    ));
    let reply = rpc.handle(&proof, &request);
    assert_eq!(reply.request_id, request.request_id());
    assert!(
        matches!(reply.outcome, ProtectedAuthorityOutcome::Resolved { ref sources, ref policies, .. } if sources == &vec!["source".to_string()] && policies.len() == 1)
    );
    for endpoint in ["cloud", "unknown"] {
        let mut denied = request.clone();
        if let ProtectedAuthorityRequest::Resolve {
            endpoint: target, ..
        } = &mut denied
        {
            *target = endpoint.into();
        }
        assert!(matches!(
            rpc.handle(&proof, &denied).outcome,
            ProtectedAuthorityOutcome::Denied
        ));
    }
    let mut modified = envelope.clone();
    modified.payload.push_str(" changed");
    let bad_cancel = ProtectedAuthorityRequest::Cancel {
        request_id: Uuid::new_v4(),
        guest: "origin-rpc".into(),
        envelope: modified,
    };
    assert!(matches!(
        rpc.handle(&origin, &bad_cancel).outcome,
        ProtectedAuthorityOutcome::Denied
    ));
    let cancel = ProtectedAuthorityRequest::Cancel {
        request_id: Uuid::new_v4(),
        guest: "origin-rpc".into(),
        envelope: envelope.clone(),
    };
    assert!(matches!(
        rpc.handle(&consumer, &cancel).outcome,
        ProtectedAuthorityOutcome::Denied
    ));
    for _ in 0..2 {
        assert!(matches!(
            rpc.handle(&origin, &cancel).outcome,
            ProtectedAuthorityOutcome::RevokedPendingQuiescence { .. }
        ));
    }
    assert!(matches!(
        rpc.handle(&proof, &request).outcome,
        ProtectedAuthorityOutcome::Denied
    ));
    assert!(LocalAuthorityRpc::new(
        "other-hotel".into(),
        f.registry.clone(),
        authority,
        BTreeMap::new()
    )
    .is_err());
}

#[tokio::test]
async fn durable_authority_receipt_denies_regrant_after_restart_and_other_issuer_cancel() {
    let mut f = Fixture::new();
    let origin = f.child("origin-durable", "agent:synthetic").await;
    let consumer = f.child("consumer-durable", "agent:consumer").await;
    let authority = f.authority(32);
    let envelope = authority
        .issue(
            origin.clone(),
            consumer.consumer(),
            Uuid::new_v4(),
            "synthetic complete payload".into(),
            Duration::from_secs(30),
        )
        .unwrap();
    // A second issuer against the same durable store cannot recreate this event.
    let second = f.authority(32);
    assert!(second
        .issue(
            origin.clone(),
            consumer.consumer(),
            envelope.task_id,
            envelope.payload.clone(),
            Duration::from_secs(30)
        )
        .is_err());
    // A durable cancellation through a separate store connection is observed.
    let connection = rusqlite::Connection::open(f.dir.path().join("synthetic.db")).unwrap();
    connection
        .execute(
            "UPDATE local_authority_receipt SET cancelled=1 WHERE handle=?1",
            [envelope.authority_handle.to_string()],
        )
        .unwrap();
    assert!(authority
        .resolve(
            &envelope,
            &consumer,
            ProcessingOperation::Inference,
            ProviderBoundary::LocalTrusted
        )
        .is_err());
    assert!(authority.pin_capture(&envelope, &consumer).is_err());
    assert!(authority
        .issue(
            origin.clone(),
            consumer.consumer(),
            envelope.task_id,
            envelope.payload.clone(),
            Duration::from_secs(30)
        )
        .is_err());
    drop(authority);
    let reopened = Arc::new(PolicyStore::open(f.dir.path().join("synthetic.db")).unwrap());
    let restarted = LocalTaskAuthority::new(
        "synthetic-hotel".into(),
        f.registry.clone(),
        reopened,
        Arc::new(Manifest),
        32,
    )
    .unwrap();
    assert!(restarted
        .issue(
            origin,
            consumer.consumer(),
            envelope.task_id,
            envelope.payload,
            Duration::from_secs(30)
        )
        .is_err());
}

#[test]
#[ignore]
fn supervised_rpc_fixture_child() {
    use philotic_client::protected_authority::{LocalCancellation, TrustedHotelPeer};
    use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let path = std::env::var("SYNTHETIC_PRIVACY_SOCKET").unwrap();
        let mut client = PhiloticClient::connect_at(
            path,
            GuestIdentity {
                guest_id: "wire-child".into(),
                role: "untrusted-administrator".into(),
                supported_tools: vec![],
            },
        )
        .await
        .unwrap();
        let response = client
            .send_request(IpcRequest::GetConfig {
                key: "synthetic-envelope".into(),
            })
            .await
            .unwrap();
        let envelope: LocalTaskEnvelope = match response {
            IpcResponse::ConfigData {
                value_json: Some(json),
                ..
            } => serde_json::from_str(&json).unwrap(),
            _ => panic!("missing synthetic fixture envelope"),
        };
        let peer = TrustedHotelPeer {
            pid: std::env::var("SYNTHETIC_HOTEL_PID")
                .unwrap()
                .parse()
                .unwrap(),
            uid: std::env::var("SYNTHETIC_HOTEL_UID")
                .unwrap()
                .parse()
                .unwrap(),
        };
        let resolution = client
            .resolve_local_authority(
                peer,
                &envelope,
                "local",
                ProcessingOperation::Inference,
                Duration::from_secs(3),
            )
            .await
            .unwrap();
        assert_eq!(resolution.actor.stable_agent_id(), "agent:synthetic");
        assert!(client
            .resolve_local_authority(
                peer,
                &envelope,
                "cloud",
                ProcessingOperation::SpeechToText,
                Duration::from_secs(3)
            )
            .await
            .is_err());
        assert_eq!(
            client
                .cancel_local_authority(peer, &envelope, Duration::from_secs(3))
                .await
                .unwrap(),
            LocalCancellation::RevokedPendingQuiescence
        );
        assert!(client
            .resolve_local_authority(
                peer,
                &envelope,
                "local",
                ProcessingOperation::Inference,
                Duration::from_secs(3)
            )
            .await
            .is_err());
    });
}

#[tokio::test]
async fn protected_rpc_real_supervised_child_uses_sdk_wire_and_canonical_issuer() {
    use ansible_mesh_core::privacy_rpc::*;
    use philotic_client::{IpcRequest, IpcResponse};
    use std::collections::BTreeMap;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn read(stream: &mut UnixStream) -> IpcRequest {
        let n = stream.read_u32().await.unwrap();
        let mut bytes = vec![0; n as usize];
        stream.read_exact(&mut bytes).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
    async fn write(stream: &mut UnixStream, response: IpcResponse) {
        let bytes = serde_json::to_vec(&response).unwrap();
        stream.write_u32(bytes.len() as u32).await.unwrap();
        stream.write_all(&bytes).await.unwrap();
    }
    let mut f = Fixture::new();
    let path = std::env::temp_dir().join(format!("p-rpc-{}.sock", Uuid::new_v4()));
    let listener = UnixListener::bind(&path).unwrap();
    let (probe, _probe_peer) = UnixStream::pair().unwrap();
    let uid = probe.peer_cred().unwrap().uid();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "supervised_rpc_fixture_child",
            "--nocapture",
        ])
        .env("SYNTHETIC_PRIVACY_SOCKET", &path)
        .env("SYNTHETIC_HOTEL_PID", std::process::id().to_string())
        .env("SYNTHETIC_HOTEL_UID", uid.to_string())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let child = Arc::new(Mutex::new(child));
    f.children.push(child.clone());
    let (mut stream, _) = listener.accept().await.unwrap();
    std::fs::remove_file(path).unwrap();
    f.registry
        .attach(
            "wire-child",
            uid,
            LaunchPrincipal {
                stable_agent_id: "agent:synthetic".into(),
                roles: BTreeSet::new(),
            },
            child.clone(),
        )
        .unwrap();
    assert!(matches!(read(&mut stream).await, IpcRequest::Register(_)));
    write(
        &mut stream,
        IpcResponse::Ack {
            req_id: "synthetic-register".into(),
        },
    )
    .await;
    let session = f
        .registry
        .authenticate("synthetic-hotel", &stream, "wire-child")
        .unwrap();
    let authority = Arc::new(f.authority(32));
    let envelope = authority
        .issue(
            session.clone(),
            session.consumer(),
            Uuid::new_v4(),
            "synthetic complete payload".into(),
            Duration::from_secs(30),
        )
        .unwrap();
    assert!(matches!(
        read(&mut stream).await,
        IpcRequest::GetConfig { .. }
    ));
    write(
        &mut stream,
        IpcResponse::ConfigData {
            key: "synthetic-envelope".into(),
            value_json: Some(serde_json::to_string(&envelope).unwrap()),
        },
    )
    .await;
    let rpc = Arc::new(
        LocalAuthorityRpc::new(
            "synthetic-hotel".into(),
            f.registry.clone(),
            authority,
            BTreeMap::from([
                ("local".into(), ProviderBoundary::LocalTrusted),
                ("cloud".into(), ProviderBoundary::External),
            ]),
        )
        .unwrap(),
    );
    for _ in 0..4 {
        let request = match tokio::time::timeout(Duration::from_secs(5), read(&mut stream))
            .await
            .unwrap()
        {
            IpcRequest::ProtectedAuthority(request) => request,
            _ => panic!("wrong protected wire request"),
        };
        let proof = rpc.authenticate(&stream, &request).unwrap();
        let server = rpc.clone();
        let response = tokio::task::spawn_blocking(move || server.handle(&proof, &request))
            .await
            .unwrap();
        write(
            &mut stream,
            IpcResponse::ProtectedAuthorityReply {
                protected_authority: response,
            },
        )
        .await;
    }
    let status = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(status) = child.lock().unwrap().try_wait().unwrap() {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn durable_cancel_waits_for_graph_reservation_without_holding_registry_lock() {
    let mut f = Fixture::new();
    let origin = f.child("origin-lease", "agent:synthetic").await;
    let consumer = f.child("consumer-lease", "agent:consumer").await;
    let authority = Arc::new(f.authority(32));
    let envelope = authority
        .issue(
            origin.clone(),
            consumer.consumer(),
            Uuid::new_v4(),
            "synthetic complete payload".into(),
            Duration::from_secs(30),
        )
        .unwrap();
    let (_, lease) = authority.pin_capture(&envelope, &consumer).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = authority.clone();
    let handle = envelope.authority_handle;
    let cancellation = std::thread::spawn(move || {
        tx.send(worker.cancel(&origin, handle)).unwrap();
    });
    assert!(rx.recv_timeout(Duration::from_millis(40)).is_err());
    // Validation must still acquire the registry while cancellation waits on SQL.
    authority
        .validate_pinned_capture(&envelope, &consumer, &lease)
        .unwrap();
    drop(lease);
    rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
    cancellation.join().unwrap();
    assert!(authority
        .resolve(
            &envelope,
            &consumer,
            ProcessingOperation::Inference,
            ProviderBoundary::LocalTrusted
        )
        .is_err());
}

#[tokio::test]
async fn durable_authority_capacity_rejects_without_evicting_tombstones() {
    let mut f = Fixture::new();
    let origin = f.child("origin-bound", "agent:synthetic").await;
    let consumer = f.child("consumer-bound", "agent:consumer").await;
    let mut connection = rusqlite::Connection::open(f.dir.path().join("synthetic.db")).unwrap();
    let transaction = connection.transaction().unwrap();
    for n in 0..4096 {
        transaction.execute("INSERT INTO local_authority_receipt VALUES ('synthetic-hotel','agent:synthetic',?1,?2,'synthetic-binding',1)", rusqlite::params![format!("synthetic-event-{n}"), Uuid::new_v4().to_string()]).unwrap();
    }
    transaction.commit().unwrap();
    assert!(f
        .authority(32)
        .issue(
            origin,
            consumer.consumer(),
            Uuid::new_v4(),
            "synthetic complete payload".into(),
            Duration::from_secs(30)
        )
        .is_err());
    let count: i64 = connection
        .query_row(
            "SELECT count(*) FROM local_authority_receipt WHERE cancelled=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 4096);
}
