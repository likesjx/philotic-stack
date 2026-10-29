use ed25519_dalek::SigningKey;
use native_memory::*;
use std::{collections::BTreeMap, sync::atomic::AtomicBool};
fn setup() -> (Ingress, Policy) {
    let key = SigningKey::from_bytes(&[7; 32]);
    let grant = Grant {
        issuer: "ingress-hotel".into(),
        reply_node: "ingress-hotel".into(),
        reply_guest: "membrane-1".into(),
        principal: "alice".into(),
        owner: "alice".into(),
        namespace: "notes".into(),
        destination: "memory-hotel".into(),
        runner: "memory-1".into(),
        operations: vec!["remember".into(), "recall".into()],
        revision: 1,
        expires_at: 1000,
        revoked: false,
    };
    let grants = BTreeMap::from([("caller-1".into(), grant)]);
    let policy = Policy {
        issuers: BTreeMap::from([(
            "ingress-hotel".into(),
            TrustedIssuer {
                key_id: "test-key".into(),
                public_key: key.verifying_key().to_bytes(),
            },
        )]),
        grants: grants.clone(),
    };
    (
        Ingress::new("ingress-hotel".into(), "test-key".into(), key, grants),
        policy,
    )
}
fn command() -> Command {
    Command::Remember {
        id: "note-1".into(),
        subject: "practice".into(),
        content: "Harmless synthetic blue notebook".into(),
        source: "synthetic://client".into(),
        source_version: "1".into(),
        lineage: "source-1".into(),
    }
}
fn issue(i: &Ingress, event: &str, nonce: &str, command: Command) -> Envelope {
    i.issue("caller-1", command, event.into(), nonce.into(), 100)
        .unwrap()
}
use native_memory::memgraph::MemgraphRunner;
use neo4rs::{Graph, query};

// Never take a graph URL from user/global configuration. This fixture may run
// only in the existing disposable CI Memgraph container, before other fixtures.
#[tokio::test]
#[ignore = "requires explicit disposable Memgraph fixture at localhost:17687"]
async fn native_memgraph_capture_transaction_acceptance() {
    assert_eq!(
        std::env::var("PHILOTIC_SYNTHETIC_NATIVE_MEMGRAPH").as_deref(),
        Ok("1")
    );
    let graph = Graph::new("127.0.0.1:17687", "", "").unwrap();
    let mut result = graph
        .execute(query("MATCH (n) RETURN count(n) AS count"))
        .await
        .unwrap();
    assert_eq!(
        result
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>("count")
            .unwrap(),
        0,
        "fixture must be empty"
    );
    let (ingress, policy) = setup();
    let runner = MemgraphRunner::new(graph.clone(), "memory-hotel".into(), "memory-1".into());
    let cancel = AtomicBool::new(false);
    let write = issue(&ingress, "event-1", "nonce-1", command());
    assert_eq!(
        runner
            .execute_current(&write, || Ok((policy.clone(), 100)), &cancel)
            .await
            .unwrap_err(),
        Error::Unavailable,
        "missing constraints fail closed"
    );
    for label in native_memory::memgraph::LABELS {
        graph
            .run(query(&format!(
                "CREATE CONSTRAINT ON (n:{label}) ASSERT n.key IS UNIQUE"
            )))
            .await
            .unwrap();
    }
    let first = runner
        .execute_current(&write, || Ok((policy.clone(), 100)), &cancel)
        .await
        .unwrap();
    let reopened = MemgraphRunner::new(
        Graph::new("127.0.0.1:17687", "", "").unwrap(),
        "memory-hotel".into(),
        "memory-1".into(),
    );
    assert_eq!(
        reopened
            .execute_current(&write, || Ok((policy.clone(), 100)), &cancel)
            .await
            .unwrap()
            .receipt,
        first.receipt
    );
    let recall = issue(
        &ingress,
        "read-1",
        "nonce-2",
        Command::Recall {
            query: "blue notebook".into(),
            limit: 10,
        },
    );
    let response = reopened
        .execute_current(&recall, || Ok((policy.clone(), 100)), &cancel)
        .await
        .unwrap();
    assert_eq!(response.observations.len(), 1);
    assert_eq!(response.observations[0].source, "synthetic://client");
    assert_eq!(response.observations[0].authenticated_writer, "alice");
    let collision = issue(&ingress, "event-2", "nonce-1", command());
    assert_eq!(
        runner
            .execute_current(&collision, || Ok((policy.clone(), 100)), &cancel)
            .await
            .unwrap_err(),
        Error::Replay
    );
    let mut changed = command();
    if let Command::Remember { content, .. } = &mut changed {
        *content = "changed".into();
    }
    let conflict = issue(&ingress, "event-1", "nonce-conflict", changed);
    assert_eq!(
        runner
            .execute_current(&conflict, || Ok((policy.clone(), 100)), &cancel)
            .await
            .unwrap_err(),
        Error::Conflict
    );
    let mut tamper = write.clone();
    tamper.request.owner = "bob".into();
    assert_eq!(
        runner
            .execute_current(&tamper, || Ok((policy.clone(), 100)), &cancel)
            .await
            .unwrap_err(),
        Error::Denied
    );
    let mut other = command();
    if let Command::Remember { id, .. } = &mut other {
        *id = "rolled-back".into();
    }
    let rollback = issue(&ingress, "rollback", "nonce-rollback", other);
    let mut calls = 0;
    assert_eq!(
        runner
            .execute_current(
                &rollback,
                || {
                    calls += 1;
                    let mut p = policy.clone();
                    if calls == 2 {
                        p.grants.get_mut("caller-1").unwrap().revoked = true;
                    }
                    Ok((p, 100))
                },
                &cancel
            )
            .await
            .unwrap_err(),
        Error::Denied
    );
    // Verify all three writes rolled back together, and exact retries did not
    // The hotel control reservation is checked asynchronously after staging and
    // before COMMIT. A denied permit must roll back observation/receipt/nonce.
    assert_eq!(
        runner
            .execute_authorized(
                &rollback,
                || Ok((policy.clone(), 100)),
                &cancel,
                async || Err(Error::Denied)
            )
            .await
            .unwrap_err(),
        Error::Denied
    );
    // Verify all three writes rolled back together, and exact retries did not
    // duplicate observations. Counts include the one committed recall receipt.
    for (label, expected) in [
        ("NativeMemoryObservation", 1),
        ("NativeMemoryReceipt", 2),
        ("NativeMemoryNonce", 2),
    ] {
        let mut rows = graph
            .execute(query(&format!(
                "MATCH (n:{label}) RETURN count(n) AS count"
            )))
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>("count")
                .unwrap(),
            expected
        );
    }
    let mut bob_grants = policy.grants.clone();
    let bob = bob_grants.get_mut("caller-1").unwrap();
    bob.owner = "bob".into();
    bob.principal = "bob".into();
    let bob_policy = Policy {
        issuers: policy.issuers.clone(),
        grants: bob_grants.clone(),
    };
    let bob_ingress = Ingress::new(
        "ingress-hotel".into(),
        "test-key".into(),
        SigningKey::from_bytes(&[7; 32]),
        bob_grants,
    );
    let bob_read = issue(
        &bob_ingress,
        "bob-read",
        "bob-nonce",
        Command::Recall {
            query: "notebook".into(),
            limit: 10,
        },
    );
    assert!(
        runner
            .execute_current(&bob_read, || Ok((bob_policy.clone(), 100)), &cancel)
            .await
            .unwrap()
            .observations
            .is_empty()
    );
    // Two concurrent deliveries cannot both create an event/observation. A
    // transaction conflict may be reported unavailable/uncertain; reconcile by
    // retrying the immutable event instead of inventing a new one.
    let mut parallel = command();
    if let Command::Remember { id, .. } = &mut parallel {
        *id = "parallel".into();
    }
    let parallel = issue(&ingress, "parallel", "nonce-parallel", parallel);
    let (a, b) = tokio::join!(
        runner.execute_current(&parallel, || Ok((policy.clone(), 100)), &cancel),
        reopened.execute_current(&parallel, || Ok((policy.clone(), 100)), &cancel)
    );
    assert!(a.is_ok() || b.is_ok());
    assert!(
        runner
            .execute_current(&parallel, || Ok((policy.clone(), 100)), &cancel)
            .await
            .is_ok()
    );
    let mut rows = graph
        .execute(query(
            "MATCH (n:NativeMemoryObservation) RETURN count(n) AS count",
        ))
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>("count")
            .unwrap(),
        2
    );
    // Remove ONLY this synthetic adapter's labels; the next existing CI fixture
    // receives an empty graph. This test never connects to a personal graph.
    for label in native_memory::memgraph::LABELS {
        graph
            .run(query(&format!("MATCH (n:{label}) DELETE n")))
            .await
            .unwrap();
        graph
            .run(query(&format!(
                "DROP CONSTRAINT ON (n:{label}) ASSERT n.key IS UNIQUE"
            )))
            .await
            .unwrap();
    }
}
