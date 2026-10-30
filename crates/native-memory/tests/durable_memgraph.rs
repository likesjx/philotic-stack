//! Two independent processes orchestrated by scripts/native-memory-durable-smoke.sh.
//! Qualifies snapshot + WAL recovery across SIGKILL, not physical power loss,
//! disk failure, host reboot, or production deployment durability.
use ed25519_dalek::SigningKey;
use native_memory::{memgraph::MemgraphRunner, *};
use neo4rs::{Graph, query};
use std::{collections::BTreeMap, sync::atomic::AtomicBool};

fn setup() -> (Graph, Ingress, Policy) {
    assert_eq!(
        std::env::var("PHILOTIC_SYNTHETIC_NATIVE_DURABLE_MEMGRAPH").as_deref(),
        Ok("1")
    );
    // Deliberately no ambient graph URI or credential discovery.
    let graph = Graph::new("127.0.0.1:17687", "", "").unwrap();
    let key = SigningKey::from_bytes(&[17; 32]);
    let grants = BTreeMap::from([(
        "synthetic-caller".into(),
        Grant {
            issuer: "synthetic-ingress".into(),
            reply_node: "synthetic-ingress".into(),
            reply_guest: "synthetic-membrane".into(),
            principal: "synthetic-owner".into(),
            owner: "synthetic-owner".into(),
            namespace: "durable-fixture".into(),
            destination: "synthetic-memory".into(),
            runner: "synthetic-runner".into(),
            operations: vec!["remember".into(), "recall".into()],
            revision: 1,
            expires_at: 1000,
            revoked: false,
        },
    )]);
    let policy = Policy {
        issuers: BTreeMap::from([(
            "synthetic-ingress".into(),
            TrustedIssuer {
                key_id: "synthetic-key".into(),
                public_key: key.verifying_key().to_bytes(),
            },
        )]),
        grants: grants.clone(),
    };
    (
        graph,
        Ingress::new(
            "synthetic-ingress".into(),
            "synthetic-key".into(),
            key,
            grants,
        ),
        policy,
    )
}

fn command() -> Command {
    Command::Remember {
        id: "synthetic-note".into(),
        subject: "synthetic notebook".into(),
        content: "Harmless synthetic durable blue notebook".into(),
        source: "synthetic://durable-fixture".into(),
        source_version: "1".into(),
        lineage: "synthetic-lineage".into(),
    }
}

fn issue(ingress: &Ingress, event: &str, nonce: &str, command: Command) -> Envelope {
    ingress
        .issue("synthetic-caller", command, event.into(), nonce.into(), 100)
        .unwrap()
}

async fn count(graph: &Graph, label: &str) -> i64 {
    let mut rows = graph
        .execute(query(&format!("MATCH (n{label}) RETURN count(n) AS count")))
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get("count").unwrap()
}

async fn counts(graph: &Graph, receipts: i64) {
    assert_eq!(count(graph, ":NativeMemoryObservation").await, 1);
    assert_eq!(count(graph, ":NativeMemoryReceipt").await, receipts);
    assert_eq!(count(graph, ":NativeMemoryNonce").await, receipts);
    assert_eq!(count(graph, "").await, 1 + 2 * receipts);
}

#[tokio::test]
#[ignore = "requires the disposable durable CI fixture; seed process only"]
async fn durable_seed() {
    let (graph, ingress, policy) = setup();
    assert_eq!(count(&graph, "").await, 0, "fixture must start empty");
    for label in native_memory::memgraph::LABELS {
        graph
            .run(query(&format!(
                "CREATE CONSTRAINT ON (n:{label}) ASSERT n.key IS UNIQUE"
            )))
            .await
            .unwrap();
    }
    // Snapshot the schema before the write: recovering the later observation,
    // receipt and nonce requires WAL recovery, not just loading the snapshot.
    graph.run(query("CREATE SNAPSHOT")).await.unwrap();
    let runner = MemgraphRunner::new(
        graph.clone(),
        "synthetic-memory".into(),
        "synthetic-runner".into(),
    );
    let response = runner
        .execute_current(
            &issue(&ingress, "durable-event", "durable-nonce", command()),
            || Ok((policy.clone(), 100)),
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
    assert_eq!(response.receipt.status, "committed");
    assert_eq!(response.receipt.committed_at, 100);
    counts(&graph, 1).await;
}

#[tokio::test]
#[ignore = "requires the same disposable volume after SIGKILL; verify process only"]
async fn durable_verify() {
    let (graph, ingress, policy) = setup();
    // Check recovered records before any operation can recreate them.
    counts(&graph, 1).await;
    let runner = MemgraphRunner::new(
        graph.clone(),
        "synthetic-memory".into(),
        "synthetic-runner".into(),
    );
    let cancel = AtomicBool::new(false);
    // Keep the original signature valid (issued at 100, expires at 160),
    // while distinguishing the recovered commit time from execution time 101.
    let response = runner
        .execute_current(
            &issue(&ingress, "durable-event", "durable-nonce", command()),
            || Ok((policy.clone(), 101)),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        response.receipt,
        Receipt {
            event: "durable-event".into(),
            status: "committed".into(),
            operation: "remember".into(),
            committed_at: 100,
        }
    );
    counts(&graph, 1).await;
    assert_eq!(
        runner
            .execute_current(
                &issue(&ingress, "different-event", "durable-nonce", command()),
                || Ok((policy.clone(), 101)),
                &cancel,
            )
            .await
            .unwrap_err(),
        Error::Replay
    );
    let mut changed = command();
    if let Command::Remember { content, .. } = &mut changed {
        *content = "Conflicting synthetic content".into();
    }
    assert_eq!(
        runner
            .execute_current(
                &issue(&ingress, "durable-event", "different-nonce", changed),
                || Ok((policy.clone(), 101)),
                &cancel,
            )
            .await
            .unwrap_err(),
        Error::Conflict
    );
    counts(&graph, 1).await;
    let recall = runner
        .execute_current(
            &issue(
                &ingress,
                "recall-event",
                "recall-nonce",
                Command::Recall {
                    query: "blue notebook".into(),
                    limit: 10,
                },
            ),
            || Ok((policy.clone(), 101)),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(recall.observations.len(), 1);
    let observation = &recall.observations[0];
    assert_eq!(observation.id, "synthetic-note");
    assert_eq!(
        observation.content,
        "Harmless synthetic durable blue notebook"
    );
    assert_eq!(observation.owner, "synthetic-owner");
    assert_eq!(observation.authenticated_writer, "synthetic-owner");
    assert_eq!(observation.source, "synthetic://durable-fixture");
    assert_eq!(observation.source_version, "1");
    assert_eq!(observation.lineage, "synthetic-lineage");
    counts(&graph, 2).await;
}
