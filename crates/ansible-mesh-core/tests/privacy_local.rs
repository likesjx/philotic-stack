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
