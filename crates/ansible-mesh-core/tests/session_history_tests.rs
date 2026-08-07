//! Session history — filtered queries + retention.
//!
//! Regression coverage for the 2026-08-07 fleet meltdown: last-N-events
//! lookups must not scan every session_event ever recorded, and retention
//! must bound how much history can accumulate at all.

use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
use ansible_mesh_core::storage::{SessionEventRecord, SessionRecord, SessionTurnRecord};
use std::sync::Arc;

struct TestStore {
    storage: SqliteGraphStorage,
    domain: GraphDomain,
}

impl std::ops::Deref for TestStore {
    type Target = GraphDomain;
    fn deref(&self) -> &GraphDomain {
        &self.domain
    }
}

fn open_graph_storage() -> TestStore {
    let storage = SqliteGraphStorage::open(":memory:").expect("open SqliteGraphStorage");
    let domain = GraphDomain::new(Arc::new(storage.adapter()));
    TestStore { storage, domain }
}

fn sample_session() -> SessionRecord {
    SessionRecord {
        session_id: "sess-1".into(),
        session_kind: "conversation".into(),
        primary_agent_id: Some("agent-jane".into()),
        active_incarnation_id: None,
        channel_kind: None,
        channel_session_key: None,
        status: "active".into(),
        lease_owner_component_id: None,
        lease_expires_at: None,
        summary_json: serde_json::json!({}),
        created_at: 100,
        updated_at: 100,
    }
}

fn make_session_event(event_id: &str, session_id: &str, created_at: u64) -> SessionEventRecord {
    SessionEventRecord {
        event_id: event_id.into(),
        session_id: session_id.into(),
        turn_id: None,
        component_id: "agent-core-test".into(),
        kind: "assistant_delta".into(),
        payload_json: serde_json::json!({"n": created_at}),
        created_at,
    }
}

fn make_turn(turn_id: &str, status: &str, started_at: u64) -> SessionTurnRecord {
    SessionTurnRecord {
        turn_id: turn_id.into(),
        session_id: "sess-1".into(),
        request_event_id: None,
        user_message_json: serde_json::json!({}),
        status: status.into(),
        response_json: None,
        error_json: None,
        started_at: Some(started_at),
        completed_at: None,
    }
}

#[test]
fn session_events_are_filtered_ordered_and_limited_in_store() {
    let store = open_graph_storage();
    // Interleave two sessions; event_ids sort differently from created_at to
    // prove ordering comes from created_at, not node_key (UUIDs are random).
    store
        .append_session_event(&make_session_event("z-old", "sess-a", 100))
        .unwrap();
    store
        .append_session_event(&make_session_event("a-new", "sess-a", 400))
        .unwrap();
    store
        .append_session_event(&make_session_event("m-mid", "sess-a", 200))
        .unwrap();
    store
        .append_session_event(&make_session_event("k-mid2", "sess-a", 300))
        .unwrap();
    store
        .append_session_event(&make_session_event("other", "sess-b", 250))
        .unwrap();

    // limit smaller than the session's history → most recent N, ascending.
    let events = store.list_session_events("sess-a", 2).unwrap();
    assert_eq!(
        events
            .iter()
            .map(|e| e.event_id.as_str())
            .collect::<Vec<_>>(),
        vec!["k-mid2", "a-new"],
        "expected the 2 most recent sess-a events ascending by created_at"
    );

    // limit 0 → everything for the session, ascending, never sess-b's.
    let events = store.list_session_events("sess-a", 0).unwrap();
    assert_eq!(
        events.iter().map(|e| e.created_at).collect::<Vec<_>>(),
        vec![100, 200, 300, 400]
    );
}

#[test]
fn zombie_turn_listing_only_sees_running_turns() {
    let store = open_graph_storage();
    store.upsert_session(&sample_session()).unwrap();
    store
        .upsert_session_turn(&make_turn("turn-done", "completed", 100))
        .unwrap();
    store
        .upsert_session_turn(&make_turn("turn-stale", "running", 100))
        .unwrap();
    store
        .upsert_session_turn(&make_turn("turn-fresh", "running", 10_000))
        .unwrap();

    let zombies = store.list_zombie_session_turns(500).unwrap();
    assert_eq!(zombies.len(), 1);
    assert_eq!(zombies[0].turn_id, "turn-stale");
}

#[test]
fn prune_session_history_deletes_old_keeps_recent_and_running() {
    let store = open_graph_storage();
    store.upsert_session(&sample_session()).unwrap();
    store
        .append_session_event(&make_session_event("evt-old", "sess-1", 100))
        .unwrap();
    store
        .append_session_event(&make_session_event("evt-new", "sess-1", 200))
        .unwrap();
    store
        .upsert_session_turn(&make_turn("turn-old-done", "completed", 100))
        .unwrap();
    store
        .upsert_session_turn(&make_turn("turn-old-running", "running", 100))
        .unwrap();
    store
        .upsert_session_turn(&make_turn("turn-new-done", "completed", 100))
        .unwrap();

    // Backdate the nodes that should age out (and the protected running turn):
    // retention keys off graph_nodes.updated_at, which is CURRENT_TIMESTAMP here.
    {
        let conn = store.storage.raw_conn().lock().unwrap();
        conn.execute(
            "UPDATE graph_nodes SET updated_at = datetime('now', '-30 days')
             WHERE node_key IN (
                 'session_event:evt-old',
                 'session_turn:sess-1:turn-old-done',
                 'session_turn:sess-1:turn-old-running'
             )",
            [],
        )
        .unwrap();
    }

    let (events, turns) = store.prune_session_history(7 * 24 * 60 * 60).unwrap();
    assert_eq!((events, turns), (1, 1), "old event + old completed turn");

    let remaining = store.list_session_events("sess-1", 0).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].event_id, "evt-new");

    // The old running turn is protected from retention (zombie repair owns it).
    assert!(store
        .get_session_turn("sess-1", "turn-old-running")
        .unwrap()
        .is_some());
    assert!(store
        .get_session_turn("sess-1", "turn-old-done")
        .unwrap()
        .is_none());
    assert!(store
        .get_session_turn("sess-1", "turn-new-done")
        .unwrap()
        .is_some());
}
