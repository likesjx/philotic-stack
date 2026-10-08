// Durable reference fixture, NOT Memgraph validation. Exercises the production
// orchestrator with real policy SQLite and a serialized file-backed graph model.
#[allow(dead_code)]
#[path = "../src/capture_commit.rs"]
mod capture_commit;
#[allow(dead_code)]
#[path = "../src/capture_local_authority.rs"]
mod capture_local_authority;
#[allow(dead_code)]
#[path = "../src/resolve_plan.rs"]
mod resolve_plan;
use ansible_mesh_core::privacy::{
    AuthenticatedAgent, ProcessingOperation, ProviderBoundary, ResourcePolicy,
    ServerAuthenticatedIdentity, authorize_processing,
};
use ansible_mesh_core::privacy_storage::*;
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use capture_commit::*;

#[test]
#[ignore]
fn authenticated_capture_child() {
    use std::io::Read;
    let mut socket =
        std::os::unix::net::UnixStream::connect(std::env::var("SYNTHETIC_CAPTURE_SOCKET").unwrap())
            .unwrap();
    let mut byte = [0];
    let _ = socket.read(&mut byte);
}
#[derive(Default)]
struct AuthenticatedChildren {
    children: Vec<Arc<Mutex<std::process::Child>>>,
    sockets: Vec<tokio::net::UnixStream>,
}
impl Drop for AuthenticatedChildren {
    fn drop(&mut self) {
        for c in &self.children {
            let mut c = c.lock().unwrap();
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}
impl AuthenticatedChildren {
    async fn launch(
        &mut self,
        _dir: &std::path::Path,
        registry: &Arc<ansible_mesh_core::privacy_local::LocalLaunchRegistry>,
        guest: &str,
        agent: &str,
    ) -> Arc<ansible_mesh_core::privacy_local::VerifiedLocalSession> {
        use ansible_mesh_core::privacy_local::*;
        let path = std::path::Path::new("/tmp")
            .join(format!("philotic-synthetic-{}.sock", ulid::Ulid::new()));
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let child = Arc::new(Mutex::new(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", "authenticated_capture_child"])
                .env("SYNTHETIC_CAPTURE_SOCKET", &path)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        ));
        let (socket, _) = listener.accept().await.unwrap();
        std::fs::remove_file(&path).unwrap();
        let uid = socket.peer_cred().unwrap().uid();
        registry
            .attach(
                guest,
                uid,
                LaunchPrincipal {
                    stable_agent_id: agent.into(),
                    roles: BTreeSet::new(),
                },
                child.clone(),
            )
            .unwrap();
        let session = registry.authenticate("synthetic", &socket, guest).unwrap();
        self.children.push(child);
        self.sockets.push(socket);
        session
    }
}
use resolve_plan::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};
struct Session;
impl ServerAuthenticatedIdentity for Session {
    fn stable_agent_id(&self) -> &str {
        "creator"
    }
    fn roles(&self) -> BTreeSet<String> {
        BTreeSet::new()
    }
}
fn actor() -> AuthenticatedAgent {
    AuthenticatedAgent::from_server(&Session).unwrap()
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
fn owner() -> AuthenticatedAgent {
    AuthenticatedAgent::from_server(&Owner).unwrap()
}
struct Authority {
    store: Arc<PolicyStore>,
    lookup: Lookup,
}
impl PlanningAuthority for Authority {
    fn authorize_capture(&self, c: &Capture) -> std::result::Result<String, Denial> {
        let record = self
            .store
            .load_pending(&actor(), &c.event.producer, &c.event.event_id)
            .map_err(|_| Denial::AccessDenied)?;
        let s = self
            .store
            .snapshot()
            .map_err(|_| Denial::MissingAuthority)?;
        authorize_processing(
            &s,
            Some(&actor()),
            &record.sources,
            ProcessingOperation::SemanticResolution,
            ProviderBoundary::LocalTrusted,
        )
        .map_err(|_| Denial::AccessDenied)?;
        Ok(s.revision().to_string())
    }
    fn resolve(&self, _: &RootAnchor) -> std::result::Result<Lookup, Denial> {
        Ok(self.lookup.clone())
    }
    fn can_read_root(&self, _: &RootId) -> bool {
        true
    }
    fn can_write(&self, _: &WriteTarget) -> bool {
        true
    }
}
#[async_trait]
impl CommitAuthority for Authority {
    async fn acquire(&self, c: &Capture, p: &CommitPayload) -> Result<CommitLease> {
        // Pin first, then validate CURRENT authority and the real persisted record.
        let lease = self.store.pin_commit()?;
        let stored = self
            .store
            .load_pending(&actor(), &c.event.producer, &c.event.event_id)?;
        let given = p.pending();
        if stored.payload != given.payload
            || stored.sources != given.sources
            || stored.recorded_by != given.recorded_by
            || stored.recorded_by != "creator"
        {
            bail!("unbound capture");
        }
        authorize_processing(
            &lease,
            Some(&actor()),
            &stored.sources,
            ProcessingOperation::SemanticResolution,
            ProviderBoundary::LocalTrusted,
        )
        .map_err(|_| anyhow!("source denied"))?;
        Ok(CommitLease {
            actor: actor(),
            revision: lease.revision().to_string(),
            hold: lease,
        })
    }
    fn validate_lease(&self, l: &CommitLease) -> Result<()> {
        if self.store.snapshot()?.revision().to_string() != l.revision {
            bail!("revision changed");
        }
        Ok(())
    }
}
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Debug)]
struct State {
    roots: BTreeMap<String, (String, String)>,
    bindings: BTreeMap<String, String>,
    extensions: BTreeMap<String, (String, String)>,
    evidence: BTreeMap<String, (String, String, String)>,
    receipts: BTreeMap<String, (String, Option<String>, String)>,
}
struct Fixture {
    path: PathBuf,
    state: Arc<tokio::sync::Mutex<State>>,
    fail_after_writes: bool,
    ready: bool,
    hook: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}
impl Fixture {
    fn open(path: PathBuf) -> Self {
        let state = if path.exists() {
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()
        } else {
            State::default()
        };
        Self {
            path,
            state: Arc::new(tokio::sync::Mutex::new(state)),
            fail_after_writes: false,
            ready: true,
            hook: Mutex::new(None),
        }
    }
    async fn state(&self) -> State {
        self.state.lock().await.clone()
    }
}
struct Tx {
    path: PathBuf,
    guard: tokio::sync::OwnedMutexGuard<State>,
    working: State,
    fail: bool,
    ready: bool,
    hook: Option<std::sync::mpsc::Sender<()>>,
}
#[async_trait]
impl AtomicLifeGraph for Fixture {
    async fn begin(&self) -> Result<Box<dyn GraphTransaction>> {
        let guard = self.state.clone().lock_owned().await;
        let working = guard.clone();
        Ok(Box::new(Tx {
            path: self.path.clone(),
            guard,
            working,
            fail: self.fail_after_writes,
            ready: self.ready,
            hook: self.hook.lock().unwrap().clone(),
        }))
    }
}
#[async_trait]
impl GraphTransaction for Tx {
    async fn schema(&mut self) -> Result<SchemaState> {
        Ok(SchemaState {
            transactional: true,
            catalog_ready: self.ready,
            unique: required_constraints(RootType::Goal),
        })
    }
    async fn receipt(&mut self, k: &str) -> Result<Option<StoredReceipt>> {
        self.working
            .receipts
            .get(k)
            .map(|(i, r, k)| {
                Ok(StoredReceipt {
                    identity: i.clone(),
                    root: r.as_ref().map(|s| RootId::parse(s).unwrap()),
                    kind: k.clone(),
                })
            })
            .transpose()
    }
    async fn resolve(&mut self, a: &RootAnchor) -> Result<Lookup> {
        Ok(match a {
            RootAnchor::VerifiedKey { namespace, value } => self
                .working
                .bindings
                .get(&opaque_key(namespace, value))
                .map(|s| Lookup::Unique(RootId::parse(s).unwrap()))
                .unwrap_or(Lookup::Absent),
            RootAnchor::ExactId(id) => {
                if self.working.roots.contains_key(id.as_str()) {
                    Lookup::Unique(id.clone())
                } else {
                    Lookup::Absent
                }
            }
            _ => Lookup::Unverified,
        })
    }
    async fn roots(&mut self, id: &RootId) -> Result<Vec<RootRecord>> {
        Ok(self
            .working
            .roots
            .get(id.as_str())
            .map(|(label, _)| {
                vec![RootRecord {
                    id: id.clone(),
                    label: label.clone(),
                }]
            })
            .unwrap_or_default())
    }
    async fn write(&mut self, w: &GraphWrite) -> Result<CommitResult> {
        let (root, kind) = match &w.disposition {
            Disposition::NewRoot {
                proposed_id,
                binding,
                ..
            } => {
                let key = opaque_key(&binding.namespace, &binding.value);
                if self.working.bindings.contains_key(&key)
                    || self.working.roots.contains_key(proposed_id.as_str())
                {
                    bail!("unique conflict");
                }
                self.working
                    .bindings
                    .insert(key, proposed_id.as_str().into());
                self.working.roots.insert(
                    proposed_id.as_str().into(),
                    (w.root_label.clone(), w.summary.clone()),
                );
                (Some(proposed_id.clone()), "new")
            }
            Disposition::Same { root } => (Some(root.clone()), "same"),
            Disposition::Extend { root } => {
                self.working.extensions.insert(
                    w.event_key.clone(),
                    (root.as_str().into(), w.details.clone().unwrap()),
                );
                (Some(root.clone()), "extension")
            }
            Disposition::Review { .. } => (None, "review"),
        };
        if let Some(r) = &root {
            for e in &w.evidence_ids {
                self.working.evidence.insert(
                    opaque_key(&w.event_key, e),
                    (r.as_str().into(), e.clone(), w.sources_json.clone()),
                );
            }
        }
        self.working.receipts.insert(
            w.event_key.clone(),
            (
                w.identity.clone(),
                root.as_ref().map(|r| r.as_str().into()),
                kind.into(),
            ),
        );
        if self.fail {
            bail!("injected failure after all writes");
        }
        if let Some(h) = &self.hook {
            h.send(())?;
        }
        Ok(CommitResult {
            root,
            kind: kind.into(),
            replay: false,
        })
    }
    async fn commit(mut self: Box<Self>) -> Result<()> {
        // Atomic durable fixture publication; this says nothing about Cypher support.
        let pending = self.path.with_extension("pending");
        let mut file = std::fs::File::create(&pending)?;
        use std::io::Write;
        file.write_all(&serde_json::to_vec(&self.working)?)?;
        file.sync_all()?;
        std::fs::rename(&pending, &self.path)?;
        std::fs::File::open(self.path.parent().unwrap())?.sync_all()?;
        *self.guard = self.working.clone();
        Ok(())
    }
    async fn rollback(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}
fn setup() -> (PathBuf, Arc<PolicyStore>, Arc<Fixture>) {
    let dir = std::env::temp_dir().join(format!("synthetic-capture-{}", ulid::Ulid::new()));
    std::fs::create_dir(&dir).unwrap();
    let store = Arc::new(PolicyStore::open(dir.join("policy.db")).unwrap());
    store
        .insert_policy(
            &owner(),
            "source",
            &ResourcePolicy::private("owner".into(), "creator".into()),
        )
        .unwrap();
    let graph = Arc::new(Fixture::open(dir.join("graph.json")));
    (dir, store, graph)
}
fn capture(store: &PolicyStore, event: &str, meaning: Meaning) -> (Capture, CommitPayload) {
    let raw=serde_json::json!({"summary":"Synthetic claim","details":"Synthetic extension","source_ids":["source"],"evidence_ids":["source"]}).to_string();
    store
        .enqueue(
            &actor(),
            PendingCapture {
                producer: "synthetic",
                event_id: event,
                payload: &raw,
                sources: &["source".into()],
                expected_revision: store.snapshot().unwrap().revision(),
            },
        )
        .unwrap();
    let c = Capture {
        event: EventKey {
            producer: "synthetic".into(),
            event_id: event.into(),
        },
        evidence_ids: BTreeSet::from(["source".into()]),
        payload_digest: capture_payload_digest(&raw),
        anchor: RootAnchor::VerifiedKey {
            namespace: "synthetic".into(),
            value: "claim".into(),
        },
        meaning,
        root_type: RootType::Goal,
        semantic_candidates: BTreeSet::new(),
    };
    let p = CommitPayload::from_pending(store.load_pending(&actor(), "synthetic", event).unwrap())
        .unwrap();
    (c, p)
}
async fn run(
    graph: &Fixture,
    a: &Authority,
    c: &Capture,
    p: &CommitPayload,
) -> Result<CommitResult> {
    let receipt = plan(c, Some(a)).map_err(|e| anyhow!("planning denied: {e:?}"))?;
    commit_capture(graph, Some(a), c, &receipt, p).await
}
#[tokio::test]
async fn durable_source_to_root_extension_evidence_receipt_and_replay() {
    let (dir, store, graph) = setup();
    let a = Authority {
        store: store.clone(),
        lookup: Lookup::Absent,
    };
    let (c, p) = capture(&store, "one", Meaning::Distinct);
    let result = run(&graph, &a, &c, &p).await.unwrap();
    let root = result.root.unwrap();
    drop(graph);
    let reopened = Fixture::open(dir.join("graph.json"));
    let a = Authority {
        store: Arc::new(PolicyStore::open(dir.join("policy.db")).unwrap()),
        lookup: Lookup::Unique(root.clone()),
    };
    assert!(run(&reopened, &a, &c, &p).await.unwrap().replay);
    let (extension, p) = capture(&store, "two", Meaning::AdditionalDetails);
    assert_eq!(
        run(&reopened, &a, &extension, &p).await.unwrap().kind,
        "extension"
    );
    let state = reopened.state().await;
    assert_eq!(
        (
            state.roots.len(),
            state.bindings.len(),
            state.extensions.len(),
            state.evidence.len(),
            state.receipts.len()
        ),
        (1, 1, 1, 2, 2)
    );
    assert_eq!(state.roots.get(root.as_str()).unwrap().1, "Synthetic claim");
    let mut conflicting = c.clone();
    conflicting.anchor = RootAnchor::ExactId(root.clone());
    let original =
        CommitPayload::from_pending(store.load_pending(&actor(), "synthetic", "one").unwrap())
            .unwrap();
    assert!(
        run(&reopened, &a, &conflicting, &original)
            .await
            .unwrap_err()
            .to_string()
            .contains("event replay conflict")
    );
    assert_eq!(reopened.state().await, state);
}
#[tokio::test]
async fn late_failure_rolls_back_every_record_and_missing_catalog_denies() {
    let (dir, store, _) = setup();
    let mut graph = Fixture::open(dir.join("graph.json"));
    graph.fail_after_writes = true;
    let a = Authority {
        store: store.clone(),
        lookup: Lookup::Absent,
    };
    let (c, p) = capture(&store, "one", Meaning::Distinct);
    assert!(run(&graph, &a, &c, &p).await.is_err());
    assert_eq!(graph.state().await, State::default());
    assert!(!dir.join("graph.json").exists());
    // Error path dropped the policy reservation as well as graph mutations.
    store.revoke_creator(&owner(), "source").unwrap();
    assert!(store.load_pending(&actor(), "synthetic", "one").is_err());
}

#[tokio::test]
async fn missing_catalog_and_authority_deny_before_writes() {
    let (dir, store, _) = setup();
    let mut graph = Fixture::open(dir.join("graph.json"));
    let a = Authority {
        store: store.clone(),
        lookup: Lookup::Absent,
    };
    let (c, p) = capture(&store, "one", Meaning::Distinct);
    graph.fail_after_writes = false;
    graph.ready = false;
    assert!(run(&graph, &a, &c, &p).await.is_err());
    assert_eq!(graph.state().await, State::default());
    let r = plan(&c, Some(&a)).unwrap();
    assert!(commit_capture(&graph, None, &c, &r, &p).await.is_err());
}

#[tokio::test]
async fn ambiguous_review_is_durable_without_root_or_evidence_mutations() {
    let (dir, store, graph) = setup();
    let (mut c, p) = capture(&store, "review", Meaning::Ambiguous);
    c.anchor = RootAnchor::Missing;
    let a = Authority {
        store: store.clone(),
        lookup: Lookup::Unverified,
    };
    let result = run(&graph, &a, &c, &p).await.unwrap();
    assert_eq!(result.kind, "review");
    assert!(result.root.is_none());
    let s = graph.state().await;
    assert_eq!(
        (
            s.roots.len(),
            s.bindings.len(),
            s.extensions.len(),
            s.evidence.len(),
            s.receipts.len()
        ),
        (0, 0, 0, 0, 1)
    );
    let reopened = Fixture::open(dir.join("graph.json"));
    assert!(run(&reopened, &a, &c, &p).await.unwrap().replay);
    store.revoke_creator(&owner(), "source").unwrap();
    assert!(run(&reopened, &a, &c, &p).await.is_err());
    assert_eq!(reopened.state().await, s);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_old_absent_plans_create_one_root_then_explicit_replan_attaches_evidence() {
    let (_, store, graph) = setup();
    let (c1, p1) = capture(&store, "one", Meaning::Distinct);
    let (c2, p2) = capture(&store, "two", Meaning::Distinct);
    let a = Arc::new(Authority {
        store: store.clone(),
        lookup: Lookup::Absent,
    });
    let r1 = plan(&c1, Some(a.as_ref())).unwrap();
    let r2 = plan(&c2, Some(a.as_ref())).unwrap();
    // Blocking SQLite acquisition must run on server blocking workers. Holding its
    // reservation while awaiting another task on a single executor would deadlock.
    let launch = |c: Capture, p: CommitPayload, r: EventReceipt| {
        let a = a.clone();
        let g = graph.clone();
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(commit_capture(g.as_ref(), Some(a.as_ref()), &c, &r, &p))
        })
    };
    let one = launch(c1.clone(), p1, r1);
    let two = launch(c2.clone(), p2, r2);
    let results = [one.join().unwrap(), two.join().unwrap()];
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let root = results
        .iter()
        .find_map(|r| r.as_ref().ok().and_then(|r| r.root.clone()))
        .unwrap();
    let mut retry = if results[0].is_err() { c1 } else { c2 };
    retry.meaning = Meaning::Same;
    let p = CommitPayload::from_pending(
        store
            .load_pending(&actor(), "synthetic", &retry.event.event_id)
            .unwrap(),
    )
    .unwrap();
    let a = Authority {
        store,
        lookup: Lookup::Unique(root),
    };
    run(&graph, &a, &retry, &p).await.unwrap();
    let s = graph.state().await;
    assert_eq!(
        (
            s.roots.len(),
            s.bindings.len(),
            s.evidence.len(),
            s.receipts.len()
        ),
        (1, 1, 2, 2)
    );
}
#[tokio::test]
async fn revocation_wins_before_acquire_denies_all_graph_writes() {
    let (_, store, graph) = setup();
    let (c, p) = capture(&store, "one", Meaning::Distinct);
    let a = Authority {
        store: store.clone(),
        lookup: Lookup::Absent,
    };
    let r = plan(&c, Some(&a)).unwrap();
    store.revoke_creator(&owner(), "source").unwrap();
    assert!(
        commit_capture(graph.as_ref(), Some(&a), &c, &r, &p)
            .await
            .is_err()
    );
    assert_eq!(graph.state().await, State::default());
}
#[tokio::test]
async fn commit_lease_serializes_revocation_through_actual_commit() {
    let (dir, store, graph) = setup();
    let (c, p) = capture(&store, "one", Meaning::Distinct);
    let a = Authority {
        store: store.clone(),
        lookup: Lookup::Absent,
    };
    let (write_tx, write_rx) = std::sync::mpsc::channel();
    *graph.hook.lock().unwrap() = Some(write_tx);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let policy_path = dir.join("policy.db");
    let revoker = std::thread::spawn(move || {
        write_rx.recv().unwrap();
        let separate = PolicyStore::open(policy_path).unwrap();
        separate.revoke_creator(&owner(), "source").unwrap();
        done_tx.send(()).unwrap();
    });
    let result = run(&graph, &a, &c, &p).await.unwrap();
    assert!(!result.replay);
    revoker.join().unwrap();
    done_rx.recv().unwrap();
    assert!(store.load_pending(&actor(), "synthetic", "one").is_err());
    assert_eq!(graph.state().await.receipts.len(), 1);
}

#[tokio::test]
async fn kernel_authenticated_local_authority_reaches_canonical_commit_and_cancel_denies_replay() {
    use ansible_mesh_core::privacy::AuthenticatedAgent;
    use ansible_mesh_core::privacy_local::*;
    use capture_local_authority::LocalCaptureAuthority;
    struct Catalog(String);
    impl LocalManifestAuthority for Catalog {
        fn sources(&self, _: Uuid, payload: &str, _: &AuthenticatedAgent) -> Option<Vec<String>> {
            (payload == self.0).then(|| vec!["source".into()])
        }
    }
    let (dir, store, graph) = setup();
    let (capture, payload) = capture(&store, "authenticated", Meaning::Distinct);
    let registry = Arc::new(LocalLaunchRegistry::default());
    let mut children = AuthenticatedChildren::default();
    let origin = children
        .launch(&dir, &registry, "producer", "creator")
        .await;
    let consumer = children
        .launch(&dir, &registry, "graph", "graph-worker")
        .await;
    let tasks = Arc::new(
        LocalTaskAuthority::new(
            "synthetic".into(),
            registry,
            store.clone(),
            Arc::new(Catalog(payload.pending().payload.clone())),
            8,
        )
        .unwrap(),
    );
    let envelope = tasks
        .issue(
            origin.clone(),
            consumer.consumer(),
            Uuid::new_v4(),
            payload.pending().payload.clone(),
            std::time::Duration::from_secs(60),
        )
        .unwrap();
    let roots = Arc::new(Authority {
        store: store.clone(),
        lookup: Lookup::Absent,
    });
    let bridge = LocalCaptureAuthority {
        tasks: tasks.clone(),
        consumer: consumer.clone(),
        envelope: envelope.clone(),
        inbox: store.clone(),
        roots,
    };
    let planned = plan(&capture, Some(&bridge)).unwrap();
    let result = commit_capture(graph.as_ref(), Some(&bridge), &capture, &planned, &payload)
        .await
        .unwrap();
    assert!(result.root.is_some());
    let state = graph.state().await;
    assert_eq!(
        (
            state.roots.len(),
            state.bindings.len(),
            state.evidence.len(),
            state.receipts.len()
        ),
        (1, 1, 1, 1)
    );
    tasks.cancel(&origin, envelope.authority_handle).unwrap();
    assert!(
        commit_capture(graph.as_ref(), Some(&bridge), &capture, &planned, &payload)
            .await
            .is_err()
    );
    assert_eq!(graph.state().await, state);
    let new = tasks
        .issue(
            origin,
            consumer.consumer(),
            Uuid::new_v4(),
            envelope.payload,
            std::time::Duration::from_secs(60),
        )
        .unwrap();
    let mut changed = new;
    changed.payload.push_str(" spoofed evidence");
    assert!(
        tasks
            .resolve(
                &changed,
                &consumer,
                ProcessingOperation::SemanticResolution,
                ProviderBoundary::LocalTrusted
            )
            .is_err()
    );
}
