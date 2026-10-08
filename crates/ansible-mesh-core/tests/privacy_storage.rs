use ansible_mesh_core::privacy::*;
use ansible_mesh_core::privacy_storage::*;
use std::collections::BTreeSet;

struct Session(&'static str);
impl ServerAuthenticatedIdentity for Session {
    fn stable_agent_id(&self) -> &str {
        self.0
    }
    fn roles(&self) -> BTreeSet<String> {
        BTreeSet::new()
    }
}
fn actor(id: &'static str) -> AuthenticatedAgent {
    AuthenticatedAgent::from_server(&Session(id)).unwrap()
}
fn setup(path: &std::path::Path) -> PolicyStore {
    let store = PolicyStore::open(path).unwrap();
    store
        .insert_policy(
            &actor("owner"),
            "source",
            &ResourcePolicy::private("owner".into(), "creator".into()),
        )
        .unwrap();
    store
}
#[test]
fn policy_and_owner_revocation_survive_reopen_and_restrict_existing_derivative() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("synthetic.db");
    let store = setup(&path);
    let mut copy = ResourcePolicy::private("creator".into(), "creator".into());
    copy.sources.push("source".into());
    store
        .insert_policy(&actor("creator"), "copy", &copy)
        .unwrap();
    let before = store.snapshot().unwrap();
    assert!(authorize_read(&before, Some(&actor("creator")), "copy").is_ok());
    assert!(store.revoke_creator(&actor("creator"), "source").is_err());
    store.revoke_creator(&actor("owner"), "source").unwrap();
    drop(store);
    let reopened = PolicyStore::open(&path).unwrap();
    let after = reopened.snapshot().unwrap();
    assert!(after.revision() > before.revision());
    assert_eq!(
        authorize_read(&after, Some(&actor("creator")), "copy"),
        Err(Denial::ReadForbidden)
    );
}
#[test]
fn insert_never_replaces_immutable_lineage_or_accepts_unknown_source_or_false_owner() {
    let dir = tempfile::tempdir().unwrap();
    let store = setup(&dir.path().join("synthetic.db"));
    let revision = store.snapshot().unwrap().revision();
    let policy = ResourcePolicy::private("owner".into(), "owner".into());
    assert!(store
        .insert_policy(&actor("owner"), "source", &policy)
        .is_err());
    assert!(store
        .insert_policy(&actor("stranger"), "new", &policy)
        .is_err());
    let mut copy = policy;
    copy.sources.push("missing".into());
    assert!(store.insert_policy(&actor("owner"), "copy", &copy).is_err());
    copy.sources = vec!["copy".into()];
    assert!(store.insert_policy(&actor("owner"), "copy", &copy).is_err());
    assert_eq!(revision, store.snapshot().unwrap().revision());
}
#[test]
fn capture_is_durable_idempotent_and_conflicting_replay_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("synthetic.db");
    let store = setup(&path);
    let sources = vec!["source".into()];
    let revision = store.snapshot().unwrap().revision();
    let make = |payload| PendingCapture {
        producer: "synthetic",
        event_id: "event-1",
        payload,
        sources: &sources,
        expected_revision: revision,
    };
    assert_eq!(
        store
            .enqueue(&actor("creator"), make("synthetic private capture"))
            .unwrap(),
        CaptureResult::Inserted
    );
    assert_eq!(
        store
            .enqueue(&actor("creator"), make("synthetic private capture"))
            .unwrap(),
        CaptureResult::Replay
    );
    assert!(store.enqueue(&actor("creator"), make("changed")).is_err());
    assert!(store
        .enqueue(&actor("owner"), make("synthetic private capture"))
        .is_err());
    assert_eq!(store.pending_count().unwrap(), 1);
    drop(store);
    let reopened = PolicyStore::open(path).unwrap();
    assert_eq!(reopened.pending_count().unwrap(), 1);
    assert_eq!(
        reopened
            .enqueue(&actor("creator"), make("synthetic private capture"))
            .unwrap(),
        CaptureResult::Replay
    );
}
#[test]
fn capture_rechecks_current_revision_and_source_acl_before_any_write_or_replay() {
    let dir = tempfile::tempdir().unwrap();
    let store = setup(&dir.path().join("synthetic.db"));
    let sources = vec!["source".into()];
    let revision = store.snapshot().unwrap().revision();
    store.revoke_creator(&actor("owner"), "source").unwrap();
    let capture = PendingCapture {
        producer: "synthetic",
        event_id: "new",
        payload: "synthetic",
        sources: &sources,
        expected_revision: revision,
    };
    assert!(store.enqueue(&actor("creator"), capture).is_err());
    let current = store.snapshot().unwrap().revision();
    assert!(store
        .enqueue(
            &actor("creator"),
            PendingCapture {
                producer: "synthetic",
                event_id: "new",
                payload: "synthetic",
                sources: &sources,
                expected_revision: current
            }
        )
        .is_err());
    assert_eq!(store.pending_count().unwrap(), 0);
}
#[test]
fn two_connections_atomically_deduplicate_same_capture() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("synthetic.db");
    let first = setup(&path);
    let second = PolicyStore::open(path).unwrap();
    let revision = first.snapshot().unwrap().revision();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let run = |store: PolicyStore, barrier: std::sync::Arc<std::sync::Barrier>| {
        std::thread::spawn(move || {
            let sources = vec!["source".into()];
            barrier.wait();
            store
                .enqueue(
                    &actor("creator"),
                    PendingCapture {
                        producer: "synthetic",
                        event_id: "concurrent",
                        payload: "synthetic",
                        sources: &sources,
                        expected_revision: revision,
                    },
                )
                .unwrap()
        })
    };
    let a = run(first, barrier.clone());
    let b = run(second, barrier);
    let outcomes = [a.join().unwrap(), b.join().unwrap()];
    assert!(outcomes.contains(&CaptureResult::Inserted));
    assert!(outcomes.contains(&CaptureResult::Replay));
    assert_eq!(
        PolicyStore::open(dir.path().join("synthetic.db"))
            .unwrap()
            .pending_count()
            .unwrap(),
        1
    );
}
#[test]
fn existing_nonprivacy_db_and_corrupt_policy_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("other.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute("CREATE TABLE synthetic_existing (id TEXT)", [])
        .unwrap();
    assert!(PolicyStore::open(&path).is_err());
    let path = dir.path().join("privacy.db");
    let store = setup(&path);
    drop(store);
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute("UPDATE privacy_policy SET policy_json='{}'", [])
        .unwrap();
    assert!(PolicyStore::open(&path).is_err());
}

#[test]
fn pending_handoff_reopens_and_rechecks_current_source_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("synthetic.db");
    let store = setup(&path);
    let sources = vec!["source".into()];
    store
        .enqueue(
            &actor("creator"),
            PendingCapture {
                producer: "synthetic",
                event_id: "handoff",
                payload: "synthetic-only",
                sources: &sources,
                expected_revision: store.snapshot().unwrap().revision(),
            },
        )
        .unwrap();
    drop(store);
    let reopened = PolicyStore::open(&path).unwrap();
    let record = reopened
        .load_pending(&actor("creator"), "synthetic", "handoff")
        .unwrap();
    assert_eq!(record.payload, "synthetic-only");
    assert_eq!(record.recorded_by, "creator");
    assert_eq!(record.sources, sources);
    assert_eq!(capture_payload_digest(&record.payload).len(), 64);
    assert!(reopened
        .load_pending(&actor("stranger"), "synthetic", "handoff")
        .is_err());
    reopened.revoke_creator(&actor("owner"), "source").unwrap();
    assert!(reopened
        .load_pending(&actor("creator"), "synthetic", "handoff")
        .is_err());
    assert_eq!(reopened.pending_count().unwrap(), 1);
}

#[test]
fn policy_commit_reservation_blocks_separate_connection_revocation_until_release() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("synthetic.db");
    let store = setup(&path);
    let lease = store.pin_commit().unwrap();
    assert!(authorize_read(&lease, Some(&actor("creator")), "source").is_ok());
    let second = PolicyStore::open(&path).unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        second.revoke_creator(&actor("owner"), "source").unwrap();
        done_tx.send(()).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(done_rx
        .recv_timeout(std::time::Duration::from_millis(100))
        .is_err());
    assert_eq!(store.snapshot().unwrap().revision(), lease.revision());
    drop(lease);
    done_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    writer.join().unwrap();
    assert!(authorize_read(
        &store.snapshot().unwrap(),
        Some(&actor("creator")),
        "source"
    )
    .is_err());
}
