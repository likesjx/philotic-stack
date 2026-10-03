//! Explicitly opt in against a disposable local database, never a hotel graph.
use data_memorygraphrag::loop_action::{Action, FIELDS, LoopAction, apply};
use neo4rs::{Graph, query};
use serde_json::{Value, json};
use std::collections::BTreeMap;

async fn snapshot(graph: &Graph, id: &str) -> BTreeMap<String, Value> {
    let columns = FIELDS
        .iter()
        .map(|k| format!("n.{k} AS {k}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut rows = graph
        .execute(query(&format!("MATCH (n {{id:$id}}) RETURN {columns}")).param("id", id))
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    FIELDS
        .iter()
        .map(|k| {
            let value = if *k == "loop_action_revision" {
                row.get::<i64>(k).map(|n| json!(n)).unwrap_or(Value::Null)
            } else {
                row.get::<String>(k)
                    .map(Value::String)
                    .unwrap_or(Value::Null)
            };
            ((*k).into(), value)
        })
        .collect()
}
fn command(id: &str, before: BTreeMap<String, Value>, action: Action) -> LoopAction {
    LoopAction {
        id: id.into(),
        actor: "edge:disposable-test".into(),
        request_id: ulid::Ulid::new().to_string() + "-retry",
        action,
        before,
        note: "operator note".into(),
    }
}

#[tokio::test]
#[ignore = "requires disposable Memgraph on localhost:17687"]
async fn audited_actions_replay_stale_aba_and_concurrent_retry() {
    let graph = Graph::new("127.0.0.1:17687", "", "").unwrap();
    let id = format!("life:loop:actions-test-{}", ulid::Ulid::new());
    graph.run(query("CREATE (:OpenLoop {id:$id,title:'Review this',validation_state:'proposed',confidence:0.4,source_membrane:'test'})").param("id",id.as_str())).await.unwrap();
    let original = snapshot(&graph, &id).await;
    let confirm = command(&id, original.clone(), Action::Confirm);
    let receipt = apply(&graph, &confirm, "2026-10-03T00:00:00Z")
        .await
        .unwrap();
    assert_eq!(receipt["status"], "saved");
    let replay = apply(&graph, &confirm, "later").await.unwrap();
    assert_eq!(replay["audit_id"], receipt["audit_id"]);
    assert_eq!(replay["replayed"], true);
    assert_eq!(
        apply(&graph, &command(&id, original, Action::Close), "now")
            .await
            .unwrap()["status"],
        "conflict"
    );
    let mut reused = command(&id, confirm.before.clone(), Action::Close);
    reused.request_id = confirm.request_id.clone();
    assert_eq!(
        apply(&graph, &reused, "now").await.unwrap()["status"],
        "conflict"
    );
    for action in [Action::Close, Action::Reopen] {
        assert_eq!(
            apply(
                &graph,
                &command(&id, snapshot(&graph, &id).await, action),
                "same-time"
            )
            .await
            .unwrap()["status"],
            "saved"
        );
    }
    let aba = snapshot(&graph, &id).await;
    for action in [Action::Close, Action::Reopen] {
        assert_eq!(
            apply(
                &graph,
                &command(&id, snapshot(&graph, &id).await, action),
                "same-time"
            )
            .await
            .unwrap()["status"],
            "saved"
        );
    }
    assert_eq!(
        apply(&graph, &command(&id, aba, Action::Close), "now")
            .await
            .unwrap()["status"],
        "conflict"
    );
    let concurrent = command(&id, snapshot(&graph, &id).await, Action::Close);
    let (a, b) = tokio::join!(
        apply(&graph, &concurrent, "race"),
        apply(&graph, &concurrent, "race")
    );
    // A database write conflict can abort one request. Retrying the *same*
    // immutable request must return the winner's receipt without another edit.
    assert!(
        a.as_ref().is_ok_and(|v| v["status"] == "saved")
            || b.as_ref().is_ok_and(|v| v["status"] == "saved")
    );
    assert_eq!(
        apply(&graph, &concurrent, "later").await.unwrap()["replayed"],
        true
    );
    let mut rows = graph
        .execute(
            query("MATCH (a:LifeLoopAction {node_id:$id}) RETURN count(a) AS count")
                .param("id", id.as_str()),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>("count")
            .unwrap(),
        6
    );
    let mut rows = graph.execute(query("MATCH (a:LifeLoopAction {id:$id}) RETURN a.before_json AS before_json,a.after_json AS after_json,a.actor AS actor,a.acted_at AS acted_at,a.note AS note,a.request_json AS request_json").param("id",confirm.audit_id())).await.unwrap();
    let audit = rows.next().await.unwrap().unwrap();
    assert_eq!(
        audit.get::<String>("actor").unwrap(),
        "edge:disposable-test"
    );
    assert_eq!(
        audit.get::<String>("acted_at").unwrap(),
        "2026-10-03T00:00:00Z"
    );
    assert_eq!(audit.get::<String>("note").unwrap(), "operator note");
    assert_eq!(
        audit.get::<String>("request_json").unwrap(),
        serde_json::to_string(&confirm).unwrap()
    );
    let before: Value = serde_json::from_str(&audit.get::<String>("before_json").unwrap()).unwrap();
    let after: Value = serde_json::from_str(&audit.get::<String>("after_json").unwrap()).unwrap();
    assert_eq!(before["validation_state"], "proposed");
    assert_eq!(after["validation_state"], "confirmed");
    let mut rows = graph
        .execute(
            query(
                "MATCH(n {id:$id}) RETURN n.confidence AS confidence,n.source_membrane AS source",
            )
            .param("id", id.as_str()),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<f64>("confidence").unwrap(), 0.4);
    assert_eq!(row.get::<String>("source").unwrap(), "test");
    // Explicit null updates remove legacy terminal aliases on real Memgraph.
    graph
        .run(
            query("MATCH(n {id:$id}) SET n.status='open',n.loop_status='resolved'")
                .param("id", id.as_str()),
        )
        .await
        .unwrap();
    assert_eq!(
        apply(
            &graph,
            &command(&id, snapshot(&graph, &id).await, Action::Reopen),
            "legacy"
        )
        .await
        .unwrap()["status"],
        "saved"
    );
    assert_eq!(snapshot(&graph, &id).await["loop_status"], Value::Null);
    graph
        .run(query("MATCH(n {id:$id}) REMOVE n:OpenLoop SET n:Goal").param("id", id.as_str()))
        .await
        .unwrap();
    assert_eq!(
        apply(
            &graph,
            &command(&id, snapshot(&graph, &id).await, Action::Close),
            "wrong-type"
        )
        .await
        .unwrap()["status"],
        "conflict"
    );
    graph
        .run(query("CREATE (:OpenLoop {id:$id})").param("id", id.as_str()))
        .await
        .unwrap();
    assert_eq!(
        apply(
            &graph,
            &command(&id, snapshot(&graph, &id).await, Action::Close),
            "ambiguous"
        )
        .await
        .unwrap()["status"],
        "conflict"
    );
    graph
        .run(query("MATCH (a:LifeLoopAction {node_id:$id}) DELETE a").param("id", id.as_str()))
        .await
        .unwrap();
    graph
        .run(query("MATCH (n {id:$id}) DELETE n").param("id", id.as_str()))
        .await
        .unwrap();
}
