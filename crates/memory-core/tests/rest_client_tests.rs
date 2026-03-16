/// Integration tests for MuninnRestEngine.
///
/// These tests require a running MuninnDB instance at http://127.0.0.1:8475.
/// They are gated behind the `MUNINN_INTEGRATION` env var to avoid failing
/// in CI environments where Muninn is not present.
///
/// Run with:
///   MUNINN_INTEGRATION=1 cargo test -p memory-core --test rest_client_tests

use memory_core::{
    AttentionalLens, LinkKind, MemoryEngine, MemoryScope, MuninnConfig, MuninnRestEngine,
    VaultResolver,
};

fn should_run() -> bool {
    std::env::var("MUNINN_INTEGRATION").is_ok()
}

fn test_engine() -> MuninnRestEngine {
    // Per-vault tokens. MuninnDB uses per-vault auth — each vault needs its own key.
    // Vault names use `_` separator: `self_{agent_id}`, `user_{user_id}`.
    // Set MUNINN_TOKEN_SELF and MUNINN_TOKEN_USER for their respective vaults.
    let self_token = std::env::var("MUNINN_TOKEN_SELF").ok();
    let user_token = std::env::var("MUNINN_TOKEN_USER").ok();

    let mut config = MuninnConfig::local("default");
    if let Some(t) = self_token {
        config = config.with_vault_token("self_test-agent-1", t.clone());
        // Also use as default token for read/forget/link (which don't know the vault)
        config.default_token = Some(t);
    }
    if let Some(t) = user_token {
        config = config.with_vault_token("user_test-user-1", t);
    }

    MuninnRestEngine::new(
        config,
        VaultResolver {
            agent_id: "test-agent-1".to_string(),
            user_id:  "test-user-1".to_string(),
        },
    )
}

#[tokio::test]
async fn remember_and_read_roundtrip() {
    if !should_run() { return; }
    let engine = test_engine();

    let eref = engine
        .remember(
            MemoryScope::SelfOnly,
            "rest-client-test",
            "This is an integration test engram for memory-core.",
            vec!["test".to_string(), "integration".to_string()],
        )
        .await
        .expect("remember");

    assert!(!eref.id.is_empty());
    assert_eq!(eref.vault_id, "self_test-agent-1");

    let engram = engine.read(&eref.id).await.expect("read").expect("engram present");
    assert_eq!(engram.concept, "rest-client-test");
    assert!(engram.content.contains("integration test"));

    // Clean up
    engine.forget(&eref.id).await.expect("forget");
}

#[tokio::test]
async fn remember_batch_returns_refs() {
    if !should_run() { return; }
    let engine = test_engine();

    let entries = vec![
        ("batch-a".to_string(), "content a".to_string(), vec!["batch".to_string()]),
        ("batch-b".to_string(), "content b".to_string(), vec!["batch".to_string()]),
    ];
    let refs = engine
        .remember_batch(MemoryScope::SharedUser, entries)
        .await
        .expect("batch");

    assert_eq!(refs.len(), 2);
    for r in &refs {
        assert!(!r.id.is_empty());
        assert_eq!(r.vault_id, "user_test-user-1");
        engine.forget(&r.id).await.expect("cleanup");
    }
}

#[tokio::test]
async fn activate_returns_results() {
    if !should_run() { return; }
    let engine = test_engine();

    // Write something we can retrieve
    let eref = engine
        .remember(
            MemoryScope::SelfOnly,
            "activate-test-memory",
            "memory core integration test for activation pipeline",
            vec!["activate-test".to_string()],
        )
        .await
        .expect("write");

    // Give MuninnDB a moment for FTS indexing (~100ms lag per docs)
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    let result = engine
        .activate("activate-test-memory integration", MemoryScope::SelfOnly, Some(5))
        .await
        .expect("activate");

    assert!(result.total > 0, "expected at least one result");
    assert!(!result.engrams.is_empty());

    engine.forget(&eref.id).await.expect("cleanup");
}

#[tokio::test]
async fn link_and_traverse() {
    if !should_run() { return; }
    let engine = test_engine();

    let a = engine.remember(MemoryScope::SelfOnly, "link-source", "source engram", vec![]).await.expect("a");
    let b = engine.remember(MemoryScope::SelfOnly, "link-target", "target engram", vec![]).await.expect("b");

    engine.link(&a.id, &b.id, LinkKind::Related).await.expect("link");

    let neighbors = engine.traverse(&a.id, Some(1)).await.expect("traverse");
    let found = neighbors.iter().any(|e| e.id == b.id);
    assert!(found, "traverse should surface the linked engram");

    engine.forget(&a.id).await.expect("cleanup a");
    engine.forget(&b.id).await.expect("cleanup b");
}

#[tokio::test]
async fn lens_auto_tags_applied_on_write() {
    if !should_run() { return; }
    let engine = test_engine();

    let lens = AttentionalLens {
        auto_tags: vec!["lens-auto-tag".to_string()],
        ..Default::default()
    };
    engine.set_lens(lens).await.expect("set_lens");

    let active = engine.current_lens().await.expect("get_lens");
    assert!(active.is_some());

    let eref = engine
        .remember(MemoryScope::SelfOnly, "lens-test", "content", vec![])
        .await
        .expect("write with lens");

    let engram = engine.read(&eref.id).await.expect("read").expect("present");
    assert!(
        engram.tags.contains(&"lens-auto-tag".to_string()),
        "auto-tag should be present on the stored engram"
    );

    engine.forget(&eref.id).await.expect("cleanup");
}

#[tokio::test]
async fn cross_scope_activate_merges_results() {
    if !should_run() { return; }
    let engine = test_engine();

    let self_ref = engine
        .remember(MemoryScope::SelfOnly, "cross-scope-self", "self vault content", vec!["cross-scope".to_string()])
        .await.expect("self write");
    let user_ref = engine
        .remember(MemoryScope::SharedUser, "cross-scope-user", "user vault content", vec!["cross-scope".to_string()])
        .await.expect("user write");

    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    let result = engine
        .activate(
            "cross-scope",
            MemoryScope::CrossScope(vec![MemoryScope::SelfOnly, MemoryScope::SharedUser]),
            Some(10),
        )
        .await
        .expect("cross-scope activate");

    assert!(result.total > 0);

    engine.forget(&self_ref.id).await.expect("cleanup self");
    engine.forget(&user_ref.id).await.expect("cleanup user");
}
