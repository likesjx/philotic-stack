//! Run only against a disposable local Memgraph on port 17687.
use data_memorygraphrag::node_edit::EDIT_QUERY;
use neo4rs::{BoltMap, BoltType, Graph, query};

fn text_map(key: &str, value: &str) -> BoltType {
    let mut map = BoltMap::new();
    map.put(key.into(), value.into());
    BoltType::Map(map)
}

#[tokio::test]
#[ignore = "requires disposable Memgraph on localhost:17687"]
async fn edit_is_atomic_audited_and_rejects_stale_originals() {
    let graph = Graph::new("127.0.0.1:17687", "", "").unwrap();
    let id = format!("life:goal:editor-test-{}", ulid::Ulid::new());
    graph
        .run(
            query("CREATE (:Goal {id: $id, title: 'Old', validation_state: 'confirmed'})")
                .param("id", id.as_str()),
        )
        .await
        .unwrap();
    for (before, after, audit_id, expected) in [
        ("Old", "New", "test-first", true),
        ("Old", "Stale", "test-stale", false),
        ("New", "Final", "test-second", true),
    ] {
        let mut rows = graph
            .execute(
                query(EDIT_QUERY)
                    .param("id", id.as_str())
                    .param("actor", "edge:test-device")
                    .param("before", text_map("title", before))
                    .param("changes", text_map("title", after))
                    .param("audit_id", format!("{id}:{audit_id}"))
                    .param("edited_at", "2026-09-16T00:00:00Z")
                    .param("before_json", format!(r#"{{"title":"{before}"}}"#))
                    .param("after_json", format!(r#"{{"title":"{after}"}}"#)),
            )
            .await
            .unwrap();
        assert_eq!(rows.next().await.unwrap().is_some(), expected);
        assert!(rows.next().await.unwrap().is_none());
    }
    let mut rows = graph
        .execute(
            query("MATCH (n {id: $id}) RETURN n.title AS title, n.validation_state AS state")
                .param("id", id.as_str()),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>("title").unwrap(), "Final");
    assert_eq!(row.get::<String>("state").unwrap(), "confirmed");
    let mut rows = graph
        .execute(
            query("MATCH (a:LifeNodeEdit {node_id: $id}) RETURN count(a) AS count")
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
        2
    );
    let mut rows = graph.execute(query(
        "MATCH (a:LifeNodeEdit {id: $id}) RETURN a.actor AS actor, a.edited_at AS edited_at, a.before_json AS before_json, a.after_json AS after_json"
    ).param("id", format!("{id}:test-first"))).await.unwrap();
    let audit = rows.next().await.unwrap().unwrap();
    assert_eq!(audit.get::<String>("actor").unwrap(), "edge:test-device");
    assert_eq!(
        audit.get::<String>("edited_at").unwrap(),
        "2026-09-16T00:00:00Z"
    );
    assert_eq!(
        audit.get::<String>("before_json").unwrap(),
        r#"{"title":"Old"}"#
    );
    assert_eq!(
        audit.get::<String>("after_json").unwrap(),
        r#"{"title":"New"}"#
    );
    // Absent originals are null, not a wildcard that can overwrite text.
    let mut rows = graph
        .execute(
            query(EDIT_QUERY)
                .param("id", id.as_str())
                .param("actor", "edge:test-device")
                .param("before", BoltType::Map(BoltMap::new()))
                .param("changes", text_map("description", "Details"))
                .param("audit_id", format!("{id}:test-null"))
                .param("edited_at", "2026-09-16T00:00:00Z")
                .param("before_json", r#"{"description":null}"#)
                .param("after_json", r#"{"description":"Details"}"#),
        )
        .await
        .unwrap();
    assert!(rows.next().await.unwrap().is_some());
    assert!(rows.next().await.unwrap().is_none());
    // Only these synthetic records are removed; never match all graph data.
    graph
        .run(query("MATCH (a:LifeNodeEdit {node_id: $id}) DELETE a").param("id", id.as_str()))
        .await
        .unwrap();
    graph
        .run(query("MATCH (n {id: $id}) DELETE n").param("id", id.as_str()))
        .await
        .unwrap();
}
