// Durable reference fixture, NOT Memgraph validation. Exercises the production
// orchestrator with real policy SQLite and a serialized file-backed graph model.
#[allow(dead_code)]
#[path = "../src/capture_commit.rs"]
mod capture_commit;
#[allow(dead_code)]
#[path = "../src/capture_local_authority.rs"]
mod capture_local_authority;
#[allow(dead_code)]
#[path = "../src/loop_action.rs"]
mod loop_action;
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
use loop_action::{Action, FIELDS, LoopAction};
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
    denied_roots: BTreeSet<RootId>,
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
    fn can_read_root(&self, root: &RootId) -> bool {
        !self.denied_roots.contains(root)
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
    lifecycle: BTreeMap<String, BTreeMap<String, serde_json::Value>>,
    roles: BTreeSet<String>,
    scoped_to: BTreeSet<(String, String)>,
    bindings: BTreeMap<String, String>,
    extensions: BTreeMap<String, (String, String)>,
    evidence: BTreeMap<String, (String, String, String)>,
    receipts: BTreeMap<String, (String, Option<String>, String)>,
    loop_actions: BTreeMap<String, String>,
}
struct Fixture {
    path: PathBuf,
    state: Arc<tokio::sync::Mutex<State>>,
    fail_after_writes: bool,
    fail_rollback: bool,
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
            fail_rollback: false,
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
    fail_rollback: bool,
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
            fail_rollback: self.fail_rollback,
            ready: self.ready,
            hook: self.hook.lock().unwrap().clone(),
        }))
    }
}
#[async_trait]
impl GraphTransaction for Tx {
    async fn schema(&mut self) -> Result<SchemaState> {
        let unique = [
            required_constraints(RootType::Goal),
            required_constraints(RootType::OpenLoop),
            required_constraints(RootType::Event),
        ]
        .into_iter()
        .flatten()
        .collect();
        Ok(SchemaState {
            transactional: true,
            catalog_ready: self.ready,
            unique,
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
                self.working.lifecycle.insert(
                    proposed_id.as_str().into(),
                    BTreeMap::from([
                        ("claim_summary".into(), serde_json::json!(w.summary)),
                        ("validation_state".into(), serde_json::json!("proposed")),
                    ]),
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
            if let Some(role_id) = &w.role_id {
                if !self.working.roles.contains(role_id) {
                    bail!("role anchor missing/ambiguous");
                }
                self.working
                    .scoped_to
                    .insert((r.as_str().into(), role_id.clone()));
            }
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
        if self.fail_rollback {
            bail!("injected rollback failure");
        }
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
    {
        let mut state = graph.state.try_lock().unwrap();
        state.roles.insert("life:role:chief-of-staff".into());
    }
    (dir, store, graph)
}
fn capture(store: &PolicyStore, event: &str, meaning: Meaning) -> (Capture, CommitPayload) {
    capture_with(
        store,
        event,
        meaning,
        RootType::Goal,
        None,
        "Synthetic claim",
        Some("Synthetic extension"),
    )
}
fn capture_with(
    store: &PolicyStore,
    event: &str,
    meaning: Meaning,
    root_type: RootType,
    role_id: Option<&str>,
    summary: &str,
    details: Option<&str>,
) -> (Capture, CommitPayload) {
    let raw = serde_json::json!({
        "summary": summary,
        "details": details,
        "source_ids": ["source"],
        "evidence_ids": ["source"],
        "role_id": role_id,
    })
    .to_string();
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
        root_type,
        semantic_candidates: BTreeSet::new(),
    };
    let p = CommitPayload::from_pending(store.load_pending(&actor(), "synthetic", event).unwrap())
        .unwrap();
    (c, p)
}
fn assert_no_capture_writes(state: &State) {
    assert!(state.roots.is_empty());
    assert!(state.lifecycle.is_empty());
    assert!(state.scoped_to.is_empty());
    assert!(state.bindings.is_empty());
    assert!(state.extensions.is_empty());
    assert!(state.evidence.is_empty());
    assert!(state.receipts.is_empty());
}
fn lifecycle_snapshot(state: &State, root: &RootId) -> BTreeMap<String, serde_json::Value> {
    let mut before: BTreeMap<String, serde_json::Value> = FIELDS
        .iter()
        .map(|k| ((*k).into(), serde_json::Value::Null))
        .collect();
    if let Some(values) = state.lifecycle.get(root.as_str()) {
        for (key, value) in values {
            before.insert(key.clone(), value.clone());
        }
    }
    before
}
async fn explicit_operator_close(
    graph: &Fixture,
    root: &RootId,
    before: BTreeMap<String, serde_json::Value>,
    request_id: &str,
) -> Result<String> {
    let action = LoopAction {
        id: root.as_str().into(),
        actor: "edge:synthetic-operator".into(),
        request_id: request_id.into(),
        action: Action::Close,
        before: before.clone(),
        note: "synthetic operator marked the loop done".into(),
    };
    let changes = action
        .changes("2026-10-13T00:00:00Z")
        .map_err(anyhow::Error::msg)?;
    let mut state = graph.state.lock().await;
    if state.loop_actions.contains_key(&action.audit_id()) {
        bail!("retry id already used");
    }
    if state
        .roots
        .get(root.as_str())
        .map(|(label, _)| label.as_str())
        != Some("OpenLoop")
        || lifecycle_snapshot(&state, root) != before
    {
        bail!("loop missing, wrong type or changed");
    }
    state
        .lifecycle
        .entry(root.as_str().into())
        .or_default()
        .extend(changes);
    state
        .loop_actions
        .insert(action.audit_id(), serde_json::to_string(&action)?);
    Ok(action.audit_id())
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
        denied_roots: BTreeSet::new(),
    };
    let (c, p) = capture(&store, "one", Meaning::Distinct);
    let result = run(&graph, &a, &c, &p).await.unwrap();
    let root = result.root.unwrap();
    drop(graph);
    let reopened = Fixture::open(dir.join("graph.json"));
    let a = Authority {
        store: Arc::new(PolicyStore::open(dir.join("policy.db")).unwrap()),
        lookup: Lookup::Unique(root.clone()),
        denied_roots: BTreeSet::new(),
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
async fn synthetic_event_creates_one_open_loop_scoped_to_existing_role_and_extension_details_attach_to_same_root()
 {
    let (dir, store, graph) = setup();
    let role = "life:role:chief-of-staff";
    let a = Authority {
        store: store.clone(),
        lookup: Lookup::Absent,
        denied_roots: BTreeSet::new(),
    };
    let (capture, payload) = capture_with(
        &store,
        "open",
        Meaning::Distinct,
        RootType::OpenLoop,
        Some(role),
        "Synthetic open loop",
        Some("Initial synthetic details"),
    );
    let result = run(&graph, &a, &capture, &payload).await.unwrap();
    assert_eq!(result.kind, "new");
    assert!(!result.replay);
    let root = result.root.unwrap();
    let state = graph.state().await;
    assert_eq!(state.roots.get(root.as_str()).unwrap().0, "OpenLoop");
    assert_eq!(
        state.scoped_to,
        BTreeSet::from([(root.as_str().to_owned(), role.to_owned())])
    );

    let reopened = Fixture::open(dir.join("graph.json"));
    let a = Authority {
        store: Arc::new(PolicyStore::open(dir.join("policy.db")).unwrap()),
        lookup: Lookup::Unique(root.clone()),
        denied_roots: BTreeSet::new(),
    };
    assert!(run(&reopened, &a, &capture, &payload).await.unwrap().replay);

    let (extension, extension_payload) = capture_with(
        &store,
        "extension",
        Meaning::AdditionalDetails,
        RootType::OpenLoop,
        Some(role),
        "Synthetic open loop",
        Some("Additional synthetic detail"),
    );
    let extension_result = run(&reopened, &a, &extension, &extension_payload)
        .await
        .unwrap();
    assert_eq!(extension_result.root, Some(root.clone()));
    assert_eq!(extension_result.kind, "extension");
    let state = reopened.state().await;
    assert_eq!(
        (
            state.roots.len(),
            state.scoped_to.len(),
            state.bindings.len(),
            state.extensions.len(),
            state.evidence.len(),
            state.receipts.len()
        ),
        (1, 1, 1, 1, 2, 2)
    );
    assert_eq!(
        state.scoped_to,
        BTreeSet::from([(root.as_str().to_owned(), role.to_owned())])
    );
}

#[tokio::test]
async fn explicit_operator_lifecycle_close_resolves_captured_open_loop() {
    let (_, store, graph) = setup();
    let role = "life:role:chief-of-staff";
    let a = Authority {
        store: store.clone(),
        lookup: Lookup::Absent,
        denied_roots: BTreeSet::new(),
    };
    let (capture, payload) = capture_with(
        &store,
        "open",
        Meaning::Distinct,
        RootType::OpenLoop,
        Some(role),
        "Synthetic open loop",
        Some("Initial synthetic details"),
    );
    let root = run(&graph, &a, &capture, &payload)
        .await
        .unwrap()
        .root
        .unwrap();
    let before = lifecycle_snapshot(&graph.state().await, &root);
    let audit_id = explicit_operator_close(&graph, &root, before.clone(), &"c".repeat(32))
        .await
        .unwrap();
    let state = graph.state().await;
    let lifecycle = state.lifecycle.get(root.as_str()).unwrap();
    assert_eq!(lifecycle["status"], serde_json::json!("resolved"));
    assert_eq!(
        lifecycle["resolution_note"],
        serde_json::json!("synthetic operator marked the loop done")
    );
    assert_eq!(state.loop_actions.len(), 1);
    assert!(state.loop_actions.contains_key(&audit_id));
    drop(state);

    assert!(
        explicit_operator_close(&graph, &root, before, &"d".repeat(32))
            .await
            .unwrap_err()
            .to_string()
            .contains("changed")
    );
}

#[tokio::test]
async fn missing_preexisting_role_anchor_rolls_back_open_loop_capture() {
    let (_, store, graph) = setup();
    let a = Authority {
        store: store.clone(),
        lookup: Lookup::Absent,
        denied_roots: BTreeSet::new(),
    };
    let (capture, payload) = capture_with(
        &store,
        "open",
        Meaning::Distinct,
        RootType::OpenLoop,
        Some("life:role:not-seeded"),
        "Synthetic open loop",
        Some("Initial synthetic details"),
    );
    assert!(run(&graph, &a, &capture, &payload).await.is_err());
    assert_no_capture_writes(&graph.state().await);
}

#[tokio::test]
async fn denied_role_anchor_authority_blocks_open_loop_before_graph_writes() {
    let (_, store, graph) = setup();
    let role = "life:role:chief-of-staff";
    let a = Authority {
        store: store.clone(),
        lookup: Lookup::Absent,
        denied_roots: BTreeSet::from([RootId::parse(role).unwrap()]),
    };
    let (capture, payload) = capture_with(
        &store,
        "open",
        Meaning::Distinct,
        RootType::OpenLoop,
        Some(role),
        "Synthetic open loop",
        Some("Initial synthetic details"),
    );
    assert!(
        run(&graph, &a, &capture, &payload)
            .await
            .unwrap_err()
            .to_string()
            .contains("role anchor denied")
    );
    assert_no_capture_writes(&graph.state().await);
}

#[tokio::test]
async fn changed_role_payload_for_same_event_is_rejected_before_commit() {
    let (_, store, _graph) = setup();
    let _ = capture_with(
        &store,
        "open",
        Meaning::Distinct,
        RootType::OpenLoop,
        Some("life:role:chief-of-staff"),
        "Synthetic open loop",
        Some("Initial synthetic details"),
    );
    let changed = serde_json::json!({
        "summary": "Synthetic open loop",
        "details": "Initial synthetic details",
        "source_ids": ["source"],
        "evidence_ids": ["source"],
        "role_id": "life:role:organist",
    })
    .to_string();
    assert!(
        store
            .enqueue(
                &actor(),
                PendingCapture {
                    producer: "synthetic",
                    event_id: "open",
                    payload: &changed,
                    sources: &["source".into()],
                    expected_revision: store.snapshot().unwrap().revision(),
                },
            )
            .unwrap_err()
            .to_string()
            .contains("event replay conflict")
    );
}

#[tokio::test]
async fn late_failure_rolls_back_every_record_and_missing_catalog_denies() {
    let (dir, store, _) = setup();
    let mut graph = Fixture::open(dir.join("graph.json"));
    graph.fail_after_writes = true;
    let a = Authority {
        store: store.clone(),
        lookup: Lookup::Absent,
        denied_roots: BTreeSet::new(),
    };
    let (c, p) = capture(&store, "one", Meaning::Distinct);
    assert!(run(&graph, &a, &c, &p).await.is_err());
    assert_no_capture_writes(&graph.state().await);
    assert!(!dir.join("graph.json").exists());
    // Error path dropped the policy reservation as well as graph mutations.
    store.revoke_creator(&owner(), "source").unwrap();
    assert!(store.load_pending(&actor(), "synthetic", "one").is_err());
}

#[tokio::test]
async fn rollback_failure_preserves_original_error_and_releases_policy() {
    let (dir, store, _) = setup();
    let mut graph = Fixture::open(dir.join("graph.json"));
    graph.fail_after_writes = true;
    graph.fail_rollback = true;
    let authority = Authority {
        store: store.clone(),
        lookup: Lookup::Absent,
        denied_roots: BTreeSet::new(),
    };
    let (capture, payload) = capture(&store, "rollback-error", Meaning::Distinct);
    let error = run(&graph, &authority, &capture, &payload)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("injected rollback failure"));
    assert_eq!(
        error.root_cause().to_string(),
        "injected failure after all writes"
    );
    assert_no_capture_writes(&graph.state().await);
    store.revoke_creator(&owner(), "source").unwrap();
}

#[tokio::test]
async fn missing_catalog_and_authority_deny_before_writes() {
    let (dir, store, _) = setup();
    let mut graph = Fixture::open(dir.join("graph.json"));
    let a = Authority {
        store: store.clone(),
        lookup: Lookup::Absent,
        denied_roots: BTreeSet::new(),
    };
    let (c, p) = capture(&store, "one", Meaning::Distinct);
    graph.fail_after_writes = false;
    graph.ready = false;
    assert!(run(&graph, &a, &c, &p).await.is_err());
    assert_no_capture_writes(&graph.state().await);
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
        denied_roots: BTreeSet::new(),
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
        denied_roots: BTreeSet::new(),
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
        denied_roots: BTreeSet::new(),
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
        denied_roots: BTreeSet::new(),
    };
    let r = plan(&c, Some(&a)).unwrap();
    store.revoke_creator(&owner(), "source").unwrap();
    assert!(
        commit_capture(graph.as_ref(), Some(&a), &c, &r, &p)
            .await
            .is_err()
    );
    assert_no_capture_writes(&graph.state().await);
}
#[tokio::test]
async fn commit_lease_serializes_revocation_through_actual_commit() {
    let (dir, store, graph) = setup();
    let (c, p) = capture(&store, "one", Meaning::Distinct);
    let a = Authority {
        store: store.clone(),
        lookup: Lookup::Absent,
        denied_roots: BTreeSet::new(),
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
        denied_roots: BTreeSet::new(),
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
