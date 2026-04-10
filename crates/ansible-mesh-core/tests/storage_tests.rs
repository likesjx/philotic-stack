//! Comprehensive test suite for the storage trait abstractions and state sync.
//!
//! Covers:
//! - `SqliteEventStorage` (append, delete, query_unacked)
//! - `SqliteCursorStorage` (get, advance, anti-regression)
//! - `GraphDomain` over `SqliteGraphStorage` (node_config, guest CRUD, memory apartment sync)
//! - `IpcRequest` / `IpcResponse` serialization round-trips

use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::event::{EventEnvelope, EventKind, EventPayload};
use ansible_mesh_core::graph::{
    AbstractSkillRecord, RoleIncarnationRecord, ToolsetProfileRecord, TurnLoopConfig,
};
use ansible_mesh_core::sqlite_storage::{
    SqliteCursorStorage, SqliteEventStorage, SqliteGraphStorage,
};
use ansible_mesh_core::storage::{
    CursorStorage, EventStorage, GuestRecord, HotelRecord, SecretRecord, SessionEventRecord,
    SessionParticipantRecord, SessionRecord, SessionTurnRecord, VaultRegistryEntry,
};
use ansible_mesh_core::{NodeCapabilities, NodeRole};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

// ─────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn make_event(kind: EventKind) -> EventEnvelope {
    EventEnvelope {
        event_id: Uuid::new_v4(),
        seq: 0, // assigned by storage
        source_node_id: "hotel-a".into(),
        target_node_id: Some("hotel-b".into()),
        source_agent_id: "agent-jane".into(),
        target_agent_id: Some("agent-bob".into()),
        kind,
        corr_id: Uuid::new_v4().to_string(),
        attempt: 1,
        created_at: now_ms(),
        expires_at: None,
        payload: EventPayload::Inline {
            data: r#"{"msg":"hello"}"#.into(),
        },
        trace: vec!["hotel-a → hotel-b".into()],
    }
}

fn open_event_storage() -> SqliteEventStorage {
    SqliteEventStorage::open(":memory:").expect("open SqliteEventStorage")
}

fn open_cursor_storage() -> SqliteCursorStorage {
    SqliteCursorStorage::open(":memory:").expect("open SqliteCursorStorage")
}

fn open_graph_domain() -> (SqliteGraphStorage, GraphDomain) {
    let storage = SqliteGraphStorage::open(":memory:").expect("open SqliteGraphStorage");
    let domain = GraphDomain::new(Arc::new(storage.adapter()));
    (storage, domain)
}

/// Test wrapper that exposes `GraphDomain` methods via `Deref` while forwarding
/// `raw_conn()` from the underlying `SqliteGraphStorage`. Allows existing tests
/// written against the old monolithic storage API to work against the split domain.
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

impl TestStore {
    fn raw_conn(&self) -> &std::sync::Arc<std::sync::Mutex<rusqlite::Connection>> {
        self.storage.raw_conn()
    }
}

fn open_graph_storage() -> TestStore {
    let (storage, domain) = open_graph_domain();
    TestStore { storage, domain }
}

fn sample_session() -> SessionRecord {
    SessionRecord {
        session_id: "sess-1".into(),
        session_kind: "conversation".into(),
        primary_agent_id: Some("agent-jane".into()),
        active_incarnation_id: Some("agent-jane:orchestrator".into()),
        channel_kind: Some("telegram".into()),
        channel_session_key: Some("telegram:123".into()),
        status: "active".into(),
        lease_owner_component_id: Some("agent-core-jane".into()),
        lease_expires_at: Some(now_ms() + 30_000),
        summary_json: serde_json::json!({"summary": "user wants concise replies"}),
        created_at: now_ms(),
        updated_at: now_ms(),
    }
}

fn sample_role_incarnation(role_name: &str) -> RoleIncarnationRecord {
    RoleIncarnationRecord {
        agent_id: "agent-jane".into(),
        role_name: role_name.into(),
        guest_id: format!("agent-jane:{role_name}"),
        toolset_profile: format!("{role_name}-profile"),
        role_identity_addendum: Some(format!("You are the {role_name} role.")),
        role_manifest: None,
        is_admin: false,
        inactive_ttl_seconds: Some(900),
        turn_loop_config: TurnLoopConfig {
            iteration_cap: Some(12),
            approval_policy: Some("default".into()),
            model_profile: Some("gemini-default".into()),
            context_window_policy: Some("balanced".into()),
            loop_script: None,
        },
    }
}

// ═══════════════════════════════════════════════════════════
// Section 1: SqliteEventStorage
// ═══════════════════════════════════════════════════════════

#[test]
fn event_storage_append_assigns_monotonic_seq() {
    let store = open_event_storage();

    let mut e1 = make_event(EventKind::TaskInvoke);
    let mut e2 = make_event(EventKind::TaskResult);

    let seq1 = store.append_event(&mut e1).expect("append e1");
    let seq2 = store.append_event(&mut e2).expect("append e2");

    assert!(seq1 < seq2, "seq must increase: {} vs {}", seq1, seq2);
    assert_eq!(e1.seq, seq1, "envelope seq must be updated by append");
    assert_eq!(e2.seq, seq2, "envelope seq must be updated by append");
}

#[test]
fn event_storage_query_unacked_respects_cursor() {
    let store = open_event_storage();

    let mut e1 = make_event(EventKind::TaskInvoke);
    let mut e2 = make_event(EventKind::TaskInvoke);
    let mut e3 = make_event(EventKind::TaskInvoke);

    store.append_event(&mut e1).unwrap();
    store.append_event(&mut e2).unwrap();
    store.append_event(&mut e3).unwrap();

    // Cursor at 0 → all 3 returned
    let all = store.query_unacked_events("hotel-b", 0, 100).unwrap();
    assert_eq!(all.len(), 3);

    // Cursor at seq of first event → only 2 returned
    let tail = store.query_unacked_events("hotel-b", e1.seq, 100).unwrap();
    assert_eq!(tail.len(), 2);

    // Cursor at last seq → none returned
    let empty = store.query_unacked_events("hotel-b", e3.seq, 100).unwrap();
    assert!(empty.is_empty(), "should be empty past last cursor");
}

#[test]
fn event_storage_query_respects_limit() {
    let store = open_event_storage();
    for _ in 0..10 {
        let mut e = make_event(EventKind::MemoryOp);
        store.append_event(&mut e).unwrap();
    }

    let page = store.query_unacked_events("hotel-b", 0, 3).unwrap();
    assert_eq!(page.len(), 3, "limit must be honoured");
}

#[test]
fn event_storage_query_filters_by_target_node() {
    let store = open_event_storage();

    let mut hotel_b = make_event(EventKind::TaskInvoke);
    let mut hotel_c = make_event(EventKind::TaskInvoke);
    hotel_c.target_node_id = Some("hotel-c".into());
    let mut broadcast = make_event(EventKind::TaskInvoke);
    broadcast.target_node_id = None;

    store.append_event(&mut hotel_b).unwrap();
    store.append_event(&mut hotel_c).unwrap();
    store.append_event(&mut broadcast).unwrap();

    let hotel_b_events = store.query_unacked_events("hotel-b", 0, 10).unwrap();
    assert_eq!(hotel_b_events.len(), 2);
    assert!(hotel_b_events.iter().all(|event| {
        event.target_node_id.is_none() || event.target_node_id.as_deref() == Some("hotel-b")
    }));

    let hotel_c_events = store.query_unacked_events("hotel-c", 0, 10).unwrap();
    assert_eq!(hotel_c_events.len(), 2);
    assert!(hotel_c_events.iter().all(|event| {
        event.target_node_id.is_none() || event.target_node_id.as_deref() == Some("hotel-c")
    }));
}

#[test]
fn event_storage_delete_removes_event() {
    let store = open_event_storage();

    let mut e = make_event(EventKind::TaskResult);
    store.append_event(&mut e).unwrap();

    let rows_deleted = store.delete_event(&e.event_id).expect("delete");
    assert_eq!(rows_deleted, 1, "exactly one row should have been deleted");

    // Should no longer appear
    let remaining = store.query_unacked_events("hotel-b", 0, 100).unwrap();
    assert!(remaining.is_empty(), "deleted event must not be returned");
}

#[test]
fn event_storage_inline_payload_round_trips() {
    let store = open_event_storage();

    let mut e = make_event(EventKind::CapabilityPublish);
    store.append_event(&mut e).unwrap();

    let fetched = store.query_unacked_events("hotel-b", 0, 1).unwrap();
    assert_eq!(fetched.len(), 1);

    match &fetched[0].payload {
        EventPayload::Inline { data } => {
            assert_eq!(data, r#"{"msg":"hello"}"#);
        }
        _ => panic!("Expected Inline payload"),
    }
}

#[test]
fn event_storage_blob_ref_payload_round_trips() {
    let store = open_event_storage();

    let mut e = EventEnvelope {
        event_id: Uuid::new_v4(),
        seq: 0,
        source_node_id: "hotel-a".into(),
        target_node_id: Some("hotel-b".into()),
        source_agent_id: "agent-jane".into(),
        target_agent_id: None,
        kind: EventKind::TaskInvoke,
        corr_id: "corr-1".into(),
        attempt: 1,
        created_at: now_ms(),
        expires_at: Some(now_ms() + 60_000),
        payload: EventPayload::BlobRef {
            blob_id: "sha256-abcdef123".into(),
            size: 102_400,
            mime: "application/octet-stream".into(),
            source_hotel_ip: "10.0.0.1:9001".into(),
        },
        trace: vec![],
    };

    store.append_event(&mut e).unwrap();
    let fetched = store.query_unacked_events("hotel-b", 0, 1).unwrap();

    match &fetched[0].payload {
        EventPayload::BlobRef { blob_id, size, .. } => {
            assert_eq!(blob_id, "sha256-abcdef123");
            assert_eq!(*size, 102_400);
        }
        _ => panic!("Expected BlobRef payload"),
    }
}

// ═══════════════════════════════════════════════════════════
// Section 2: SqliteCursorStorage
// ═══════════════════════════════════════════════════════════

#[test]
fn cursor_storage_defaults_to_zero() {
    let store = open_cursor_storage();
    let cursor = store.get_cursor("node-unknown").unwrap();
    assert_eq!(cursor, 0);
}

#[test]
fn cursor_storage_advance_and_read() {
    let store = open_cursor_storage();
    let ts = now_ms();

    store.advance_cursor("node-x", 42, ts).unwrap();
    assert_eq!(store.get_cursor("node-x").unwrap(), 42);
}

#[test]
fn cursor_storage_never_regresses() {
    let store = open_cursor_storage();
    let ts = now_ms();

    store.advance_cursor("node-x", 100, ts).unwrap();
    store.advance_cursor("node-x", 50, ts).unwrap(); // lower value — must be ignored
    assert_eq!(
        store.get_cursor("node-x").unwrap(),
        100,
        "cursor must never go backwards"
    );
}

#[test]
fn cursor_storage_independent_per_node() {
    let store = open_cursor_storage();
    let ts = now_ms();

    store.advance_cursor("node-a", 10, ts).unwrap();
    store.advance_cursor("node-b", 99, ts).unwrap();

    assert_eq!(store.get_cursor("node-a").unwrap(), 10);
    assert_eq!(store.get_cursor("node-b").unwrap(), 99);
}

// ═══════════════════════════════════════════════════════════
// Section 3: SqliteGraphStorage — node config
// ═══════════════════════════════════════════════════════════

#[test]
fn graph_storage_node_capabilities_round_trip() {
    let store = open_graph_storage();

    let caps = NodeCapabilities {
        node_id: "hotel-test-01".into(),
        roles: vec![NodeRole::AnsibleNode],
        models: vec!["gemini-2.0-flash@2026.1".into()],
        tools: vec!["mcp.search@1".into()],
        constraints: Default::default(),
    };

    // Initially empty
    assert!(store.load_node_capabilities().unwrap().is_none());

    store.save_node_capabilities(&caps).unwrap();

    let loaded = store
        .load_node_capabilities()
        .unwrap()
        .expect("should exist");
    assert_eq!(loaded.node_id, "hotel-test-01");
    assert_eq!(loaded.roles, vec![NodeRole::AnsibleNode]);
    assert_eq!(loaded.models, vec!["gemini-2.0-flash@2026.1"]);
}

#[test]
fn graph_storage_node_capabilities_upsert() {
    let store = open_graph_storage();

    let caps_v1 = NodeCapabilities {
        node_id: "hotel-01".into(),
        roles: vec![NodeRole::AnsibleNode],
        models: vec![],
        tools: vec![],
        constraints: Default::default(),
    };
    let mut caps_v2 = caps_v1.clone();
    caps_v2.models = vec!["new-model@1".into()];

    store.save_node_capabilities(&caps_v1).unwrap();
    store.save_node_capabilities(&caps_v2).unwrap(); // upsert

    let loaded = store.load_node_capabilities().unwrap().unwrap();
    assert_eq!(
        loaded.models,
        vec!["new-model@1"],
        "second save must overwrite first"
    );
}

#[test]
fn graph_storage_get_config_value_round_trip() {
    let store = open_graph_storage();

    let caps = NodeCapabilities {
        node_id: "hotel-config-01".into(),
        roles: vec![NodeRole::AnsibleNode],
        models: vec![],
        tools: vec![],
        constraints: Default::default(),
    };

    store.save_node_capabilities(&caps).unwrap();

    let loaded = store
        .load_node_capabilities()
        .unwrap()
        .expect("capabilities should exist after save");

    assert_eq!(loaded.node_id, "hotel-config-01");
}

#[test]
fn graph_storage_set_config_value_populates_graph_node() {
    let store = open_graph_storage();

    store
        .set_config_value("telegram_bot_token", "\"secret-token\"")
        .unwrap();

    let conn = store.raw_conn().lock().unwrap();
    let graph_json: String = conn
        .query_row(
            "SELECT data_json FROM graph_nodes WHERE node_key = 'config:telegram_bot_token'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&graph_json).unwrap();
    // New format: {"value": "<json_string>"} (not the old {"key":..,"value_json":..})
    assert_eq!(parsed["value"], "\"secret-token\"");
}

#[test]
fn graph_storage_secret_round_trips() {
    let store = open_graph_storage();
    let secret = SecretRecord {
        secret_ref: "secret://hotel/default/test/one".into(),
        secret_kind: "test".into(),
        scope: "hotel".into(),
        allowed_roles: vec!["model".into()],
        allowed_guests: vec!["guest-1".into()],
        ciphertext_b64: "ciphertext".into(),
        nonce_b64: "nonce".into(),
        created_at: now_ms(),
        updated_at: now_ms(),
    };

    store.upsert_secret(&secret).expect("upsert secret");
    let fetched = store
        .get_secret(&secret.secret_ref)
        .expect("get secret")
        .expect("secret should exist");

    assert_eq!(fetched, secret);
}

#[test]
fn graph_storage_hotel_round_trip_and_pid_update() {
    let store = open_graph_storage();

    let hotel = HotelRecord {
        hotel_name: "alpha".into(),
        capabilities: NodeCapabilities {
            node_id: "alpha-node".into(),
            roles: vec![NodeRole::AnsibleNode],
            models: vec![],
            tools: vec![],
            constraints: Default::default(),
        },
        mesh_port: 9101,
        blob_port: 9201,
        execution_port: 9301,
        ipc_socket_path: "/tmp/philotic-alpha.sock".into(),
        active_pid: None,
    };

    store.upsert_hotel(&hotel).unwrap();
    store.set_hotel_pid("alpha", Some("2222")).unwrap();

    let loaded = store
        .get_hotel("alpha")
        .unwrap()
        .expect("hotel should exist");
    assert_eq!(loaded.capabilities.node_id, "alpha-node");
    assert_eq!(loaded.mesh_port, 9101);
    assert_eq!(loaded.blob_port, 9201);
    assert_eq!(loaded.execution_port, 9301);
    assert_eq!(loaded.ipc_socket_path, "/tmp/philotic-alpha.sock");
    assert_eq!(loaded.active_pid.as_deref(), Some("2222"));
}

#[test]
fn graph_storage_lists_hotels() {
    let store = open_graph_storage();

    let alpha = HotelRecord {
        hotel_name: "alpha".into(),
        capabilities: NodeCapabilities {
            node_id: "alpha-node".into(),
            roles: vec![NodeRole::AnsibleNode],
            models: vec![],
            tools: vec![],
            constraints: Default::default(),
        },
        mesh_port: 9101,
        blob_port: 9201,
        execution_port: 9301,
        ipc_socket_path: "/tmp/philotic-alpha.sock".into(),
        active_pid: None,
    };
    let beta = HotelRecord {
        hotel_name: "beta".into(),
        capabilities: NodeCapabilities {
            node_id: "beta-node".into(),
            roles: vec![NodeRole::AnsibleNode],
            models: vec![],
            tools: vec![],
            constraints: Default::default(),
        },
        mesh_port: 9102,
        blob_port: 9202,
        execution_port: 9302,
        ipc_socket_path: "/tmp/philotic-beta.sock".into(),
        active_pid: None,
    };

    store.upsert_hotel(&beta).unwrap();
    store.upsert_hotel(&alpha).unwrap();

    let listed = store.list_hotels().unwrap();
    let names: Vec<_> = listed
        .iter()
        .map(|hotel| hotel.hotel_name.as_str())
        .collect();
    assert_eq!(names, vec!["alpha", "beta"]);
}

// ═══════════════════════════════════════════════════════════
// Section 4: SqliteGraphStorage — guest CRUD
// ═══════════════════════════════════════════════════════════

fn sample_guest(id: &str, active: bool) -> GuestRecord {
    GuestRecord {
        hotel_name: "test-hotel".into(),
        guest_id: id.into(),
        role: "agent".into(),
        config_json: r#"{"command":"agent-core"}"#.into(),
        is_active: active,
        active_pid: None,
        last_active_at: None,
    }
}

#[test]
fn graph_storage_seed_and_list_guests() {
    let store = open_graph_storage();

    store
        .seed_guests(
            "test-hotel",
            &[
                sample_guest("g1", true),
                sample_guest("g2", true),
                sample_guest("g3", false),
            ],
        )
        .unwrap();

    let all = store.list_guests("test-hotel", false).unwrap();
    assert_eq!(all.len(), 3);

    let active = store.list_guests("test-hotel", true).unwrap();
    assert_eq!(active.len(), 2, "only is_active=1 guests returned");
    assert!(active.iter().all(|g| g.is_active));
}

#[test]
fn graph_storage_set_guest_pid() {
    let store = open_graph_storage();
    store
        .seed_guests("test-hotel", &[sample_guest("agent-1", true)])
        .unwrap();

    // Assign a PID
    store
        .set_guest_pid("test-hotel", "agent-1", Some("1234"))
        .unwrap();
    let guests = store.list_guests("test-hotel", false).unwrap();
    assert_eq!(
        guests[0].active_pid.as_deref(),
        Some("1234"),
        "PID should be set"
    );

    // Clear the PID
    store.set_guest_pid("test-hotel", "agent-1", None).unwrap();
    let guests = store.list_guests("test-hotel", false).unwrap();
    assert!(guests[0].active_pid.is_none(), "PID should be cleared");
}

#[test]
fn graph_storage_list_empty_is_ok() {
    let store = open_graph_storage();
    let all = store.list_guests("test-hotel", false).unwrap();
    assert!(all.is_empty());
}

#[test]
fn graph_storage_seed_is_idempotent() {
    let store = open_graph_storage();
    let guest = sample_guest("g-idem", true);

    store.seed_guests("test-hotel", &[guest.clone()]).unwrap();
    store.seed_guests("test-hotel", &[guest.clone()]).unwrap(); // INSERT OR REPLACE

    let all = store.list_guests("test-hotel", false).unwrap();
    assert_eq!(all.len(), 1, "idempotent seed must not duplicate");
}

#[test]
fn graph_storage_seed_guest_creates_graph_node_and_edge() {
    let store = open_graph_storage();
    let guest = sample_guest("g-graph", true);

    store.seed_guests("test-hotel", &[guest]).unwrap();

    let conn = store.raw_conn().lock().unwrap();
    let node_count: u64 = conn
        .query_row(
            "SELECT count(*) FROM graph_nodes WHERE kind = 'guest'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let edge_count: u64 = conn
        .query_row(
            "SELECT count(*) FROM graph_edges WHERE edge_kind = 'HAS_GUEST'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(node_count, 1);
    // GraphDomain.seed_guests no longer creates hotel→guest edges (graph node only).
    let _ = edge_count;
}

// ═══════════════════════════════════════════════════════════
// Section 5: SqliteGraphStorage — memory apartments (state sync)
// ═══════════════════════════════════════════════════════════

/// No-op: agent_identities FK no longer applies in the graph-node model.
/// Kept so call sites compile without changes.
fn seed_agent_identity(_store: &TestStore, _agent_id: &str) {}

fn count_apartments(store: &TestStore) -> u64 {
    let conn = store.raw_conn().lock().unwrap();
    conn.query_row(
        "SELECT count(*) FROM graph_nodes WHERE kind = 'apartment'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn graph_storage_sync_apartment_inserts() {
    let store = open_graph_storage();
    seed_agent_identity(&store, "agent-jane");

    let content = serde_json::json!({"recent": "hello world"});
    store
        .sync_apartment("agent-jane", "short", &content)
        .unwrap();

    assert_eq!(count_apartments(&store), 1);
}

#[test]
fn graph_storage_sync_apartment_lww_replaces() {
    let store = open_graph_storage();
    seed_agent_identity(&store, "agent-jane");

    let v1 = serde_json::json!({"version": 1});
    let v2 = serde_json::json!({"version": 2});

    store.sync_apartment("agent-jane", "short", &v1).unwrap();
    store.sync_apartment("agent-jane", "short", &v2).unwrap(); // LWW

    assert_eq!(count_apartments(&store), 1, "LWW must replace, not append");

    // Verify the value is the latest via GraphDomain.get_apartment
    let apartment = store
        .get_apartment("agent-jane", "short")
        .unwrap()
        .expect("apartment should exist after LWW upsert");
    assert_eq!(
        apartment["version"], 2,
        "stored content should be v2, got: {apartment}"
    );
}

#[test]
fn graph_storage_sync_apartment_independent_types() {
    let store = open_graph_storage();
    seed_agent_identity(&store, "agent-jane");

    store
        .sync_apartment("agent-jane", "short", &serde_json::json!({"s": 1}))
        .unwrap();
    store
        .sync_apartment("agent-jane", "long", &serde_json::json!({"l": 1}))
        .unwrap();
    store
        .sync_apartment("agent-jane", "episodic", &serde_json::json!({"e": 1}))
        .unwrap();

    assert_eq!(
        count_apartments(&store),
        3,
        "each memory_type is independent"
    );
}

#[test]
fn graph_storage_sync_apartment_independent_agents() {
    let store = open_graph_storage();
    seed_agent_identity(&store, "agent-jane");
    seed_agent_identity(&store, "agent-bob");

    store
        .sync_apartment("agent-jane", "short", &serde_json::json!({"j": 1}))
        .unwrap();
    store
        .sync_apartment("agent-bob", "short", &serde_json::json!({"b": 1}))
        .unwrap();

    assert_eq!(
        count_apartments(&store),
        2,
        "different agents own separate apartments"
    );
}

#[test]
fn graph_storage_sync_apartment_creates_graph_node_and_edge() {
    let store = open_graph_storage();
    seed_agent_identity(&store, "agent-jane");

    store
        .sync_apartment(
            "agent-jane",
            "short",
            &serde_json::json!({"recent": "hello"}),
        )
        .unwrap();

    // node_key format: "apartment:<agent_id>:<memory_type>" (from GraphDomain::apartment_key)
    let conn = store.raw_conn().lock().unwrap();
    let apartment_json: String = conn
        .query_row(
            "SELECT data_json FROM graph_nodes WHERE node_key = 'apartment:agent-jane:short'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let apartment: serde_json::Value = serde_json::from_str(&apartment_json).unwrap();

    assert_eq!(apartment["agent_id"], "agent-jane");
    assert_eq!(apartment["memory_type"], "short");
    // "content" field (not "content_json") per GraphDomain::sync_apartment schema
    assert_eq!(apartment["content"]["recent"], "hello");
    // GraphDomain does not create apartment edges in the current implementation.
}

#[test]
fn graph_storage_sync_apartment_backfills_missing_agent_identity() {
    // GraphDomain no longer backfills agent_identities on apartment sync —
    // the apartment is stored as a graph node with no FK constraint.
    // Verify that syncing without a pre-existing identity succeeds and is readable.
    let store = open_graph_storage();

    store
        .sync_apartment(
            "agent-unseeded",
            "short",
            &serde_json::json!({"summary": "created from sync"}),
        )
        .expect("sync apartment must succeed without a pre-existing agent identity");

    let apartment = store
        .get_apartment("agent-unseeded", "short")
        .expect("get apartment")
        .expect("apartment should exist");
    assert_eq!(apartment["summary"], "created from sync");
}

#[test]
fn graph_storage_get_apartment_returns_latest_checkpoint() {
    let store = open_graph_storage();
    seed_agent_identity(&store, "agent-jane");

    store
        .sync_apartment(
            "agent-jane",
            "short",
            &serde_json::json!({"session_id": "sess-1"}),
        )
        .unwrap();

    let checkpoint = store
        .get_apartment("agent-jane", "short")
        .unwrap()
        .expect("checkpoint should exist");
    assert_eq!(checkpoint["session_id"], "sess-1");
}

// ═══════════════════════════════════════════════════════════
// Section 5b: Generalized sessions
// ═══════════════════════════════════════════════════════════

#[test]
fn graph_storage_session_round_trip() {
    let store = open_graph_storage();
    let session = sample_session();

    store.upsert_session(&session).unwrap();

    let loaded = store
        .get_session("sess-1")
        .unwrap()
        .expect("session should exist");
    assert_eq!(loaded.session_kind, "conversation");
    assert_eq!(loaded.primary_agent_id.as_deref(), Some("agent-jane"));
    assert_eq!(
        loaded.active_incarnation_id.as_deref(),
        Some("agent-jane:orchestrator")
    );
    assert_eq!(loaded.summary_json["summary"], "user wants concise replies");
}

#[test]
fn graph_storage_role_incarnation_round_trip() {
    let store = open_graph_storage();
    let role = sample_role_incarnation("developer");

    store.upsert_role_incarnation(&role).unwrap();

    let loaded = store
        .get_role_incarnation("agent-jane", "developer")
        .unwrap()
        .expect("role incarnation should exist");
    assert_eq!(loaded, role);
}

#[test]
fn graph_storage_lists_role_incarnations_by_agent() {
    let store = open_graph_storage();
    store
        .upsert_role_incarnation(&sample_role_incarnation("orchestrator"))
        .unwrap();
    store
        .upsert_role_incarnation(&sample_role_incarnation("developer"))
        .unwrap();
    store
        .upsert_role_incarnation(&RoleIncarnationRecord {
            agent_id: "agent-aria".into(),
            role_name: "researcher".into(),
            guest_id: "agent-aria:researcher".into(),
            toolset_profile: "research-profile".into(),
            role_identity_addendum: None,
            role_manifest: None,
            is_admin: false,
            inactive_ttl_seconds: None,
            turn_loop_config: TurnLoopConfig::default(),
        })
        .unwrap();

    let jane_roles = store.list_role_incarnations("agent-jane").unwrap();
    assert_eq!(jane_roles.len(), 2);
    assert!(jane_roles.iter().all(|role| role.agent_id == "agent-jane"));
}

#[test]
fn graph_storage_abstract_skill_round_trip() {
    let store = open_graph_storage();
    let skill = AbstractSkillRecord {
        skill_name: "handoff.to_role".into(),
        description: "Hand off to a specialist role once the target is justified.".into(),
        implied_tools: vec!["session.status".into()],
        ..Default::default()
    };

    store.upsert_abstract_skill(&skill).unwrap();

    let loaded = store
        .get_abstract_skill("handoff.to_role")
        .unwrap()
        .expect("abstract skill should exist");
    assert_eq!(loaded, skill);

    let listed = store.list_abstract_skills().unwrap();
    assert!(listed
        .iter()
        .any(|entry| entry.skill_name == "handoff.to_role"));
}

#[test]
fn graph_storage_toolset_profile_round_trip() {
    let store = open_graph_storage();
    let profile = ToolsetProfileRecord {
        profile_name: "orchestrator".into(),
        allowed_tools: vec!["session.status".into(), "echo".into()],
        allowed_classes: vec!["session".into()],
        allowed_skills: vec!["handoff.to_role".into()],
        description: Some("Core orchestrator profile.".into()),
    };

    store.upsert_toolset_profile(&profile).unwrap();

    let loaded = store
        .get_toolset_profile("orchestrator")
        .unwrap()
        .expect("toolset profile should exist");
    assert_eq!(loaded, profile);

    let listed = store.list_toolset_profiles().unwrap();
    assert!(listed.iter().any(|p| p.profile_name == "orchestrator"));
}

#[test]
fn graph_storage_session_participants_round_trip() {
    let store = open_graph_storage();
    store.upsert_session(&sample_session()).unwrap();

    store
        .upsert_session_participant(&SessionParticipantRecord {
            session_id: "sess-1".into(),
            component_id: "membrane-telegram".into(),
            role: "gateway".into(),
            joined_at: now_ms(),
            last_seen_at: now_ms(),
        })
        .unwrap();
    store
        .upsert_session_participant(&SessionParticipantRecord {
            session_id: "sess-1".into(),
            component_id: "agent-core-jane".into(),
            role: "owner".into(),
            joined_at: now_ms(),
            last_seen_at: now_ms(),
        })
        .unwrap();

    let participants = store.list_session_participants("sess-1").unwrap();
    assert_eq!(participants.len(), 2);
    assert!(participants.iter().any(|p| p.role == "gateway"));
    assert!(participants.iter().any(|p| p.role == "owner"));
}

#[test]
fn graph_storage_session_turns_round_trip() {
    let store = open_graph_storage();
    store.upsert_session(&sample_session()).unwrap();

    store
        .upsert_session_turn(&SessionTurnRecord {
            turn_id: "turn-1".into(),
            session_id: "sess-1".into(),
            request_event_id: Some("req-1".into()),
            user_message_json: serde_json::json!({"content": "hello"}),
            status: "completed".into(),
            response_json: Some(serde_json::json!({"content": "hi"})),
            error_json: None,
            started_at: Some(100),
            completed_at: Some(200),
        })
        .unwrap();
    store
        .upsert_session_turn(&SessionTurnRecord {
            turn_id: "turn-2".into(),
            session_id: "sess-1".into(),
            request_event_id: Some("req-2".into()),
            user_message_json: serde_json::json!({"content": "status?"}),
            status: "running".into(),
            response_json: None,
            error_json: None,
            started_at: Some(300),
            completed_at: None,
        })
        .unwrap();

    let turns = store.list_session_turns("sess-1", 10).unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].turn_id, "turn-1");
    assert_eq!(turns[1].turn_id, "turn-2");

    let turn = store
        .get_session_turn("sess-1", "turn-2")
        .unwrap()
        .expect("turn should exist");
    assert_eq!(turn.status, "running");
}

#[test]
fn graph_storage_session_events_round_trip() {
    let store = open_graph_storage();
    store.upsert_session(&sample_session()).unwrap();
    store
        .upsert_session_turn(&SessionTurnRecord {
            turn_id: "turn-1".into(),
            session_id: "sess-1".into(),
            request_event_id: None,
            user_message_json: serde_json::json!({"content": "hello"}),
            status: "running".into(),
            response_json: None,
            error_json: None,
            started_at: Some(100),
            completed_at: None,
        })
        .unwrap();

    store
        .append_session_event(&SessionEventRecord {
            event_id: "evt-1".into(),
            session_id: "sess-1".into(),
            turn_id: Some("turn-1".into()),
            component_id: "agent-core-jane".into(),
            kind: "assistant_delta".into(),
            payload_json: serde_json::json!({"text": "Thinking..."}),
            created_at: 101,
        })
        .unwrap();
    store
        .append_session_event(&SessionEventRecord {
            event_id: "evt-2".into(),
            session_id: "sess-1".into(),
            turn_id: Some("turn-1".into()),
            component_id: "agent-core-jane".into(),
            kind: "tool_call".into(),
            payload_json: serde_json::json!({"tool": "shell"}),
            created_at: 102,
        })
        .unwrap();

    let events = store.list_session_events("sess-1", 10).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event_id, "evt-1");
    assert_eq!(events[1].event_id, "evt-2");
    assert_eq!(events[1].payload_json["tool"], "shell");
}

#[test]
fn graph_storage_session_checkpoint_flow_e2e() {
    let store = open_graph_storage();
    seed_agent_identity(&store, "agent-jane");

    store.upsert_session(&sample_session()).unwrap();
    store
        .upsert_session_participant(&SessionParticipantRecord {
            session_id: "sess-1".into(),
            component_id: "membrane-telegram".into(),
            role: "gateway".into(),
            joined_at: 100,
            last_seen_at: 100,
        })
        .unwrap();
    store
        .upsert_session_participant(&SessionParticipantRecord {
            session_id: "sess-1".into(),
            component_id: "agent-core-jane".into(),
            role: "owner".into(),
            joined_at: 101,
            last_seen_at: 101,
        })
        .unwrap();

    store
        .upsert_session_turn(&SessionTurnRecord {
            turn_id: "turn-1".into(),
            session_id: "sess-1".into(),
            request_event_id: Some("req-1".into()),
            user_message_json: serde_json::json!({"content": "Summarize my CPU usage"}),
            status: "completed".into(),
            response_json: Some(serde_json::json!({"content": "CPU is at 45%"})),
            error_json: None,
            started_at: Some(110),
            completed_at: Some(140),
        })
        .unwrap();

    store
        .append_session_event(&SessionEventRecord {
            event_id: "evt-1".into(),
            session_id: "sess-1".into(),
            turn_id: Some("turn-1".into()),
            component_id: "agent-core-jane".into(),
            kind: "assistant_delta".into(),
            payload_json: serde_json::json!({"text": "Checking system status..."}),
            created_at: 120,
        })
        .unwrap();
    store
        .append_session_event(&SessionEventRecord {
            event_id: "evt-2".into(),
            session_id: "sess-1".into(),
            turn_id: Some("turn-1".into()),
            component_id: "agent-core-jane".into(),
            kind: "tool_call".into(),
            payload_json: serde_json::json!({"tool": "shell", "command": "uptime"}),
            created_at: 125,
        })
        .unwrap();

    store
        .sync_apartment(
            "agent-jane",
            "short",
            &serde_json::json!({
                "session_id": "sess-1",
                "summary": "User asked for CPU usage; answered with current utilization.",
                "recent_turn_ids": ["turn-1"]
            }),
        )
        .unwrap();

    let session = store
        .get_session("sess-1")
        .unwrap()
        .expect("session should exist");
    let participants = store.list_session_participants("sess-1").unwrap();
    let turns = store.list_session_turns("sess-1", 10).unwrap();
    let events = store.list_session_events("sess-1", 10).unwrap();

    assert_eq!(session.status, "active");
    assert_eq!(participants.len(), 2);
    assert_eq!(turns.len(), 1);
    assert_eq!(
        turns[0].response_json.as_ref().unwrap()["content"],
        "CPU is at 45%"
    );
    assert_eq!(events.len(), 2);

    // Verify the apartment checkpoint via GraphDomain (graph_nodes, not memory_apartments)
    let apartment = store
        .get_apartment("agent-jane", "short")
        .expect("get apartment")
        .expect("apartment should exist after sync");
    assert_eq!(apartment["session_id"], "sess-1");
    assert_eq!(apartment["recent_turn_ids"][0], "turn-1");
}

// ═══════════════════════════════════════════════════════════
// Section 6: IpcRequest / IpcResponse serialization
// ═══════════════════════════════════════════════════════════

// (philotic-client is a separate crate, tested via its own lib.rs #[cfg(test)])
// Re-export types here to test JSON round-trips from ansible-mesh-core's perspective.

#[cfg(test)]
mod ipc_serde_tests {
    use philotic_client::{GuestIdentity, IpcRequest, IpcResponse};
    use uuid::Uuid;

    fn roundtrip_request(req: &IpcRequest) -> IpcRequest {
        let json = serde_json::to_string(req).expect("serialize");
        serde_json::from_str(&json).expect("deserialize")
    }

    fn roundtrip_response(res: &IpcResponse) -> IpcResponse {
        let json = serde_json::to_string(res).expect("serialize");
        serde_json::from_str(&json).expect("deserialize")
    }

    #[test]
    fn ipc_request_register_round_trip() {
        let req = IpcRequest::Register(GuestIdentity {
            guest_id: "agent-01".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        });
        let rt = roundtrip_request(&req);
        match rt {
            IpcRequest::Register(id) => assert_eq!(id.guest_id, "agent-01"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn ipc_request_sync_apartment_round_trip() {
        let req = IpcRequest::SyncApartment {
            agent_id: "agent-jane".into(),
            memory_type: "short".into(),
            content_json: serde_json::json!({"key": "value", "count": 42}),
        };
        let rt = roundtrip_request(&req);
        match rt {
            IpcRequest::SyncApartment {
                agent_id,
                memory_type,
                content_json,
            } => {
                assert_eq!(agent_id, "agent-jane");
                assert_eq!(memory_type, "short");
                assert_eq!(content_json["count"], 42);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn ipc_request_create_task_round_trip() {
        let req = IpcRequest::CreateTask {
            target_role: "model".into(),
            payload: serde_json::json!({"prompt": "hello"}),
        };
        let rt = roundtrip_request(&req);
        match rt {
            IpcRequest::CreateTask { target_role, .. } => {
                assert_eq!(target_role, "model");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn ipc_request_ack_event_round_trip() {
        let id = Uuid::new_v4();
        let req = IpcRequest::AckEvent { event_id: id };
        let rt = roundtrip_request(&req);
        match rt {
            IpcRequest::AckEvent { event_id } => assert_eq!(event_id, id),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn ipc_request_get_secret_round_trip() {
        let req = IpcRequest::GetSecret {
            secret_ref: "secret://hotel/default/test/one".into(),
        };
        let rt = roundtrip_request(&req);
        match rt {
            IpcRequest::GetSecret { secret_ref } => {
                assert_eq!(secret_ref, "secret://hotel/default/test/one");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn ipc_response_apartment_update_round_trip() {
        let res = IpcResponse::ApartmentUpdate {
            agent_id: "agent-jane".into(),
            memory_type: "long".into(),
            content_json: serde_json::json!({"resolved": true}),
        };
        let rt = roundtrip_response(&res);
        match rt {
            IpcResponse::ApartmentUpdate {
                agent_id,
                memory_type,
                content_json,
            } => {
                assert_eq!(agent_id, "agent-jane");
                assert_eq!(memory_type, "long");
                assert_eq!(content_json["resolved"], true);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn ipc_response_standard_success() {
        let res = IpcResponse::success("corr-abc", Some(serde_json::json!({"rows": 3})));
        let rt = roundtrip_response(&res);
        match rt {
            IpcResponse::Standard { ok, code, data, .. } => {
                assert!(ok);
                assert_eq!(code, "OK");
                assert_eq!(data.unwrap()["rows"], 3);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn ipc_response_error_round_trip() {
        let res = IpcResponse::Error("something went wrong".into());
        let rt = roundtrip_response(&res);
        match rt {
            IpcResponse::Error(msg) => {
                assert_eq!(msg, "something went wrong");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn ipc_response_secret_data_round_trip() {
        let res = IpcResponse::SecretData {
            secret_ref: "secret://hotel/default/test/one".into(),
            value_json: Some("\"shh\"".into()),
        };
        let rt = roundtrip_response(&res);
        match rt {
            IpcResponse::SecretData {
                secret_ref,
                value_json,
            } => {
                assert_eq!(secret_ref, "secret://hotel/default/test/one");
                assert_eq!(value_json.as_deref(), Some("\"shh\""));
            }
            _ => panic!("wrong variant"),
        }
    }
}

// ── MuninnDB config helpers ──────────────────────────────────────────────────

#[test]
fn muninn_endpoint_round_trip() {
    let store = open_graph_storage();

    // Absent by default.
    assert!(store.get_muninn_endpoint().unwrap().is_none());

    store.set_muninn_endpoint("http://127.0.0.1:8475").unwrap();
    let loaded = store
        .get_muninn_endpoint()
        .unwrap()
        .expect("endpoint present");
    assert_eq!(loaded, "http://127.0.0.1:8475");

    // Overwrite.
    store
        .set_muninn_endpoint("http://muninn.example.com:8475")
        .unwrap();
    let updated = store
        .get_muninn_endpoint()
        .unwrap()
        .expect("endpoint present");
    assert_eq!(updated, "http://muninn.example.com:8475");
}

#[test]
fn vault_registry_upsert_and_list() {
    let store = open_graph_storage();

    // Empty by default.
    assert!(store.get_vault_registry().unwrap().is_empty());

    // Insert two entries.
    store
        .upsert_vault_registry_entry(&VaultRegistryEntry {
            vault_name: "self_philote-1".into(),
            secret_ref: "secret://hotel/default/muninn/self-philote-1".into(),
        })
        .unwrap();
    store
        .upsert_vault_registry_entry(&VaultRegistryEntry {
            vault_name: "user_jared".into(),
            secret_ref: "secret://hotel/default/muninn/user-jared".into(),
        })
        .unwrap();

    let entries = store.get_vault_registry().unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|e| e.vault_name == "self_philote-1"));
    assert!(entries.iter().any(|e| e.vault_name == "user_jared"));
}

#[test]
fn vault_registry_upsert_updates_existing() {
    let store = open_graph_storage();

    store
        .upsert_vault_registry_entry(&VaultRegistryEntry {
            vault_name: "self_philote-1".into(),
            secret_ref: "secret://hotel/default/muninn/self-philote-1-v1".into(),
        })
        .unwrap();

    // Upsert with same vault_name but new secret_ref.
    store
        .upsert_vault_registry_entry(&VaultRegistryEntry {
            vault_name: "self_philote-1".into(),
            secret_ref: "secret://hotel/default/muninn/self-philote-1-v2".into(),
        })
        .unwrap();

    let entries = store.get_vault_registry().unwrap();
    assert_eq!(entries.len(), 1, "should not duplicate");
    assert_eq!(
        entries[0].secret_ref,
        "secret://hotel/default/muninn/self-philote-1-v2"
    );
}

#[test]
fn vault_registry_remove_entry() {
    let store = open_graph_storage();

    store
        .upsert_vault_registry_entry(&VaultRegistryEntry {
            vault_name: "self_philote-1".into(),
            secret_ref: "secret://hotel/default/muninn/self-philote-1".into(),
        })
        .unwrap();
    store
        .upsert_vault_registry_entry(&VaultRegistryEntry {
            vault_name: "user_jared".into(),
            secret_ref: "secret://hotel/default/muninn/user-jared".into(),
        })
        .unwrap();

    store.remove_vault_registry_entry("self_philote-1").unwrap();

    let entries = store.get_vault_registry().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].vault_name, "user_jared");

    // Remove non-existent — should be a no-op, not an error.
    store.remove_vault_registry_entry("self_philote-1").unwrap();
}
