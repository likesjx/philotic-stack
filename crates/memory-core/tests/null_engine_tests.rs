use memory_core::{MemoryEngine, MemoryScope, NullMemoryEngine, RecallContext, RecallTrigger};

// ──── NullMemoryEngine: error contract ───────────────────────────────────────
//
// Every MemoryEngine operation on the null stub must return an error.
// These tests verify the trait contract is fully implemented and that
// the stub fails loudly rather than silently.

#[tokio::test]
async fn null_remember_errors() {
    let engine = NullMemoryEngine;
    let result = engine
        .remember(MemoryScope::SelfOnly, "test", "content", vec![])
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("NullMemoryEngine"));
}

#[tokio::test]
async fn null_remember_batch_errors() {
    let engine = NullMemoryEngine;
    let result = engine.remember_batch(MemoryScope::SharedUser, vec![]).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn null_activate_errors() {
    let engine = NullMemoryEngine;
    let result = engine
        .activate("some context", MemoryScope::SelfOnly, None)
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn null_evaluate_recall_uses_default_policy_without_backend() {
    let engine = NullMemoryEngine;
    let decision = engine
        .evaluate_recall(&RecallContext {
            trigger: RecallTrigger::UserTurnStart,
            scope: MemoryScope::SelfOnly,
            recall_seed_text: "Continue the memory design work".into(),
            active_goal: None,
            role_name: None,
            recent_turns: Vec::new(),
            local_memory_summaries: Vec::new(),
            tool_history_summary: Vec::new(),
            lens: None,
        })
        .await
        .expect("default deterministic policy should not require backend");

    assert!(decision.should_recall());
    assert_eq!(decision.limit, Some(5));
}

#[tokio::test]
async fn null_recall_for_turn_errors_when_activation_is_required() {
    let engine = NullMemoryEngine;
    let result = engine
        .recall_for_turn(&RecallContext {
            trigger: RecallTrigger::UserTurnStart,
            scope: MemoryScope::SelfOnly,
            recall_seed_text: "Continue the memory design work".into(),
            active_goal: None,
            role_name: None,
            recent_turns: Vec::new(),
            local_memory_summaries: Vec::new(),
            tool_history_summary: Vec::new(),
            lens: None,
        })
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("NullMemoryEngine"));
}

#[tokio::test]
async fn null_read_errors() {
    let engine = NullMemoryEngine;
    let result = engine.read(&"fake-id".to_string()).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn null_forget_errors() {
    let engine = NullMemoryEngine;
    let result = engine.forget(&"fake-id".to_string()).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn null_link_errors() {
    use memory_core::LinkKind;
    let engine = NullMemoryEngine;
    let result = engine
        .link(&"a".to_string(), &"b".to_string(), LinkKind::Related)
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn null_traverse_errors() {
    let engine = NullMemoryEngine;
    let result = engine.traverse(&"fake-id".to_string(), None).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn null_lens_errors() {
    use memory_core::AttentionalLens;
    let engine = NullMemoryEngine;
    let set_result = engine.set_lens(AttentionalLens::default()).await;
    assert!(set_result.is_err());
    let get_result = engine.current_lens().await;
    assert!(get_result.is_err());
}

#[tokio::test]
async fn null_subscribe_errors_with_phase5_message() {
    let engine = NullMemoryEngine;
    let result = engine.subscribe("context", MemoryScope::SelfOnly).await;
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Phase 5"),
        "expected Phase 5 message, got: {err}"
    );
}

// ──── MemoryScope: serde round-trip ──────────────────────────────────────────

#[test]
fn memory_scope_serde_roundtrip() {
    let scopes = vec![
        memory_core::MemoryScope::SelfOnly,
        memory_core::MemoryScope::SharedUser,
        memory_core::MemoryScope::Session("sess-123".to_string()),
        memory_core::MemoryScope::CrossScope(vec![
            memory_core::MemoryScope::SelfOnly,
            memory_core::MemoryScope::SharedUser,
        ]),
    ];
    for scope in scopes {
        let json = serde_json::to_string(&scope).expect("serialize");
        let _back: memory_core::MemoryScope = serde_json::from_str(&json).expect("deserialize");
    }
}

// ──── CognitiveOutcome: serde round-trip ─────────────────────────────────────

#[test]
fn cognitive_outcome_serde_roundtrip() {
    use memory_core::CognitiveOutcome;
    let outcomes = vec![
        CognitiveOutcome::ResolvedContradiction {
            description: "X contradicted Y".to_string(),
            resolution: "X supersedes Y".to_string(),
            engram_ids: vec!["e1".to_string()],
        },
        CognitiveOutcome::SolidifiedBelief {
            concept: "user-prefers-direct-feedback".to_string(),
            content: "User consistently responds better to direct, unhedged answers.".to_string(),
            confidence: 0.85,
        },
        CognitiveOutcome::MetacognitiveObservation {
            observation: "Struggled with ambiguous scope boundaries in this turn.".to_string(),
        },
        CognitiveOutcome::RejectedApproach {
            description: "Attempted incremental refactor".to_string(),
            reason: "Hot-file overlap with another active workstream.".to_string(),
        },
    ];
    for outcome in outcomes {
        let json = serde_json::to_string(&outcome).expect("serialize");
        let _back: CognitiveOutcome = serde_json::from_str(&json).expect("deserialize");
    }
}
