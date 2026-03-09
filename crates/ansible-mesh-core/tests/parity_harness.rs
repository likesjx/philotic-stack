use ansible_mesh_core::authz::{MeshAuth, NonceTracker};
use ansible_mesh_core::cursor::CursorTracker;
use ansible_mesh_core::event::{EventEnvelope, EventKind, EventPayload};
use ansible_mesh_core::ledger::EventLedger;
use std::time::{SystemTime, UNIX_EPOCH};

const TEST_PSK: &str = "SHADOW_PARITY_TEST_KEY_2026";

/// Helper function to generate a fake payload and event
fn mock_event(id: uuid::Uuid, seq: u64) -> EventEnvelope {
    EventEnvelope {
        event_id: id,
        seq,
        source_node_id: "node-test-1".to_string(),
        target_node_id: Some("node-test-2".to_string()),
        source_agent_id: "agent-a".to_string(),
        target_agent_id: Some("agent-b".to_string()),
        kind: EventKind::TaskInvoke,
        corr_id: "test-corr-1".to_string(),
        attempt: 1,
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        expires_at: None,
        payload: EventPayload::Inline {
            data: r#"{"hello":"world"}"#.to_string(),
        },
        trace: vec![],
    }
}

#[tokio::test]
async fn test_authz_crypto_validation_and_time_bound() {
    let auth = MeshAuth::new(TEST_PSK);
    let msg_id = uuid::Uuid::new_v4();
    let seq = 1;
    let payload = b"test_payload_data";

    // 1. Success case
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let valid_hmac = auth.sign(&msg_id, seq, payload, now);
    assert!(
        auth.validate(&msg_id, seq, payload, now, &valid_hmac)
            .is_ok(),
        "Valid HMAC should pass"
    );

    // 2. Tampered Payload (Forgery)
    let bad_payload = b"tampered_payload_data";
    assert!(
        auth.validate(&msg_id, seq, bad_payload, now, &valid_hmac)
            .is_err(),
        "Invalid payload must fail HMAC"
    );

    // 3. Invalid Time Window (Replay window bypass test)
    let old_timestamp = now - 600; // 10 minutes ago
                                   // Even if legitimately signed 10 minutes ago, it must be rejected now
    let old_hmac = auth.sign(&msg_id, seq, payload, old_timestamp);
    assert!(
        auth.validate(&msg_id, seq, payload, old_timestamp, &old_hmac)
            .is_err(),
        "Expired timestamp must fail time window checks"
    );
}

#[test]
fn test_nonce_replay_guard() {
    // In-memory sqlite DB for test
    let tracker = NonceTracker::open(":memory:").expect("Failed to open volatile DB");
    let msg_id = uuid::Uuid::new_v4();

    // 1. First seen is allowed
    assert!(
        tracker.assert_and_record_nonce(&msg_id).is_ok(),
        "Fresh nonce must be accepted"
    );

    // 2. Exact same seen again is instantly blocked by UNIQUE constraint
    let result = tracker.assert_and_record_nonce(&msg_id);
    assert!(result.is_err(), "Replayed nonce must be rejected");

    let err_str = result.unwrap_err().to_string();
    assert!(
        err_str.contains("Replay detected"),
        "Error message string should contain replay detection wrapper indication: {}",
        err_str
    );
}

#[test]
fn test_ledger_idempotent_deduplication() {
    let ledger = EventLedger::open(":memory:").expect("Failed to open volatile DB");

    let evt1 = mock_event(uuid::Uuid::new_v4(), 1);
    let evt2 = mock_event(uuid::Uuid::new_v4(), 2);

    // 1. Apply events sequentially
    ledger
        .append_event(&mut evt1.clone())
        .expect("Append event 1");
    ledger
        .append_event(&mut evt2.clone())
        .expect("Append event 2");

    // Check count
    let count: u64 = ledger
        .conn
        .lock()
        .unwrap()
        .query_row("SELECT count(*) FROM mesh_events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 2, "Ledger should have 2 events");

    // 2. Apply same exact events again (Replay simulation on control-plane ledger)
    let _res1 = ledger.append_event(&mut evt1.clone());
    let _res2 = ledger.append_event(&mut evt2.clone());

    // The SQLite insert ignores conflicts on event_id. Check they did not append rows.
    let count_after: u64 = ledger
        .conn
        .lock()
        .unwrap()
        .query_row("SELECT count(*) FROM mesh_events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        count_after, 2,
        "Ledger idempotency failed: duplicate events were stored!"
    );
}

#[test]
fn test_cursor_advancement() {
    let tracker = CursorTracker::open(":memory:").expect("Failed to open volatile DB");
    let consumer = "consumer_node_alpha";

    // 1. Uninitialized
    assert_eq!(
        tracker.get_cursor(consumer).unwrap(),
        0,
        "Default cursor should be 0"
    );

    // 2. Advance
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    tracker
        .advance_cursor(consumer, 10, ts)
        .expect("Advance cursor to 10");
    assert_eq!(
        tracker.get_cursor(consumer).unwrap(),
        10,
        "Cursor should be 10"
    );

    // 3. Advance further
    tracker
        .advance_cursor(consumer, 25, ts)
        .expect("Advance cursor to 25");
    assert_eq!(
        tracker.get_cursor(consumer).unwrap(),
        25,
        "Cursor should be 25"
    );
}
