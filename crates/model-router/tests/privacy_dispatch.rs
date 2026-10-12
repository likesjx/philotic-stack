use ansible_mesh_core::privacy::*;
use ansible_mesh_core::privacy_storage::PolicyStore;
use anyhow::{Result, bail};
use async_trait::async_trait;
use model_router::controller::{ControllerTask, ModelProvider, ProviderOutput};
use model_router::privacy_dispatch::*;
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

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

struct FixtureAuthority {
    store: Arc<PolicyStore>,
    registered: Mutex<Option<ControllerTask>>,
    sources: Vec<String>,
    expected_revision: Option<u64>,
}
impl DispatchPrivacyAuthority for FixtureAuthority {
    fn context_for(&self, task: &ControllerTask) -> Option<VerifiedDispatchContext> {
        if self.registered.lock().ok()?.as_ref()? != task {
            return None;
        }
        let policies = self.store.snapshot().ok()?;
        if self
            .expected_revision
            .is_some_and(|v| v != policies.revision())
        {
            return None;
        }
        Some(VerifiedDispatchContext {
            actor: actor("creator"),
            policies,
            sources: self.sources.clone(),
        })
    }
}
struct MockProvider {
    calls: AtomicUsize,
    fail: bool,
    hang: bool,
}
#[async_trait]
impl ModelProvider for MockProvider {
    fn id(&self) -> &'static str {
        "mock"
    }
    fn supports(&self, _: &ControllerTask) -> bool {
        true
    }
    fn supports_streaming(&self, _: &ControllerTask) -> bool {
        true
    }
    async fn invoke(&self, _: &ControllerTask) -> Result<ProviderOutput> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.hang {
            std::future::pending::<()>().await;
        }
        if self.fail {
            bail!("synthetic failure");
        }
        Ok(ProviderOutput::Text {
            content: "synthetic response".into(),
            display_text: Some("synthetic response".into()),
            spoken_text: None,
            partial_replies: vec![],
            working_memory_delta: None,
            follow_up_questions: vec![],
            intent_summary: None,
            memory_concept: None,
            memory_candidate: None,
            active_plan: None,
            model_gen: None,
        })
    }
    async fn invoke_streaming(
        &self,
        task: &ControllerTask,
        tokens: tokio::sync::mpsc::Sender<String>,
    ) -> Result<ProviderOutput> {
        tokens.send("synthetic-token".into()).await?;
        self.invoke(task).await
    }
}
fn task() -> ControllerTask {
    ControllerTask::from_value(&task_payload()).unwrap()
}
fn task_payload() -> serde_json::Value {
    json!({"kind":"text.generate", "prompt":"synthetic private request", "context": {"memory":[{"text":"synthetic private summary"}], "tool_history":[{"tool_name":"synthetic", "arguments":{}, "result":"synthetic private tool result"}]}, "effective_rights":["tool.synthetic"], "tools_for_model":[{"tool_name":"synthetic", "description":"synthetic private tool context"}]})
}
fn setup(dir: &std::path::Path, task: ControllerTask) -> Arc<FixtureAuthority> {
    let store = Arc::new(PolicyStore::open(dir.join("synthetic.db")).unwrap());
    store
        .insert_policy(
            &actor("owner"),
            "source",
            &ResourcePolicy::private("owner".into(), "creator".into()),
        )
        .unwrap();
    Arc::new(FixtureAuthority {
        store,
        registered: Mutex::new(Some(task)),
        sources: vec!["source".into()],
        expected_revision: None,
    })
}
fn mock(fail: bool) -> Arc<MockProvider> {
    Arc::new(MockProvider {
        calls: AtomicUsize::new(0),
        fail,
        hang: false,
    })
}

#[tokio::test]
async fn actual_provider_trait_denies_private_external_payload_with_zero_invocations() {
    let dir = tempfile_dir();
    let task = task();
    let authority = setup(&dir, task.clone());
    let inner = mock(false);
    let registry = guarded_registry(vec![(inner.clone(), ProviderBoundary::External)], authority);
    assert!(
        registry
            .resolve(&task)
            .unwrap()
            .invoke(&task)
            .await
            .unwrap_err()
            .to_string()
            .contains("privacy_denied")
    );
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn local_failure_retry_and_cloud_fallback_cannot_bypass_final_gate() {
    let dir = tempfile_dir();
    let task = task();
    let authority = setup(&dir, task.clone());
    let local = mock(true);
    let cloud = mock(false);
    let local = PrivacyBoundProvider::new(
        local.clone(),
        authority.clone(),
        ProviderBoundary::LocalTrusted,
    );
    let fallback = PrivacyBoundProvider::new(cloud.clone(), authority, ProviderBoundary::External);
    for _ in 0..2 {
        assert!(local.invoke(&task).await.is_err());
        assert!(fallback.invoke(&task).await.is_err());
    }
    assert_eq!(cloud.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn revocation_between_attempts_rechecks_persisted_source_policy() {
    let dir = tempfile_dir();
    let task = task();
    let authority = setup(&dir, task.clone());
    let inner = mock(false);
    let guarded = PrivacyBoundProvider::new(
        inner.clone(),
        authority.clone(),
        ProviderBoundary::LocalTrusted,
    );
    guarded.invoke(&task).await.unwrap();
    authority
        .store
        .revoke_creator(&actor("owner"), "source")
        .unwrap();
    assert!(guarded.invoke(&task).await.is_err());
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn streaming_denial_emits_no_tokens_and_unknown_endpoint_denies() {
    let dir = tempfile_dir();
    let task = task();
    let authority = setup(&dir, task.clone());
    for boundary in [ProviderBoundary::External, ProviderBoundary::Unknown] {
        let inner = mock(false);
        let guarded = PrivacyBoundProvider::new(inner.clone(), authority.clone(), boundary);
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        assert!(guarded.invoke_streaming(&task, tx).await.is_err());
        assert!(rx.recv().await.is_none());
        assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn spoofed_payload_flags_missing_context_or_changed_summary_and_tools_do_not_authenticate() {
    let dir = tempfile_dir();
    let task = task();
    let authority = setup(&dir, task.clone());
    let inner = mock(false);
    let guarded = PrivacyBoundProvider::new(
        inner.clone(),
        authority.clone(),
        ProviderBoundary::LocalTrusted,
    );
    let spoof = ControllerTask::from_value(&json!({"kind":"text.generate", "prompt":"synthetic private request", "agent_id":"owner", "private":false, "egress":"allow"})).unwrap();
    assert!(guarded.invoke(&spoof).await.is_err());
    let mut changed = task.clone();
    changed.prompt = Some("changed synthetic tool-result summary".into());
    assert!(guarded.invoke(&changed).await.is_err());
    changed = task.clone();
    changed.tools.clear();
    assert!(guarded.invoke(&changed).await.is_err());
    changed = task.clone();
    changed.context.memory[0].text = Some("changed summary".into());
    assert!(guarded.invoke(&changed).await.is_err());
    changed = task.clone();
    changed.context.tool_history[0].result = "changed tool result".into();
    assert!(guarded.invoke(&changed).await.is_err());
    *authority.registered.lock().unwrap() = None;
    assert!(guarded.invoke(&task).await.is_err());
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn stale_revision_missing_source_and_native_live_paths_deny() {
    let dir = tempfile_dir();
    let task = task();
    let original = setup(&dir, task.clone());
    let authority = Arc::new(FixtureAuthority {
        store: original.store.clone(),
        registered: Mutex::new(Some(task.clone())),
        sources: vec!["missing".into()],
        expected_revision: None,
    });
    let inner = mock(false);
    assert!(
        PrivacyBoundProvider::new(inner.clone(), authority, ProviderBoundary::LocalTrusted)
            .invoke(&task)
            .await
            .is_err()
    );
    let stale = Arc::new(FixtureAuthority {
        store: original.store.clone(),
        registered: Mutex::new(Some(task.clone())),
        sources: vec!["source".into()],
        expected_revision: Some(0),
    });
    assert!(
        PrivacyBoundProvider::new(inner.clone(), stale, ProviderBoundary::LocalTrusted)
            .invoke(&task)
            .await
            .is_err()
    );
    let native =
        ControllerTask::from_value(&json!({"kind":"response.generate","prompt":"synthetic"}))
            .unwrap();
    *original.registered.lock().unwrap() = Some(native.clone());
    assert!(
        PrivacyBoundProvider::new(inner.clone(), original, ProviderBoundary::LocalTrusted)
            .invoke(&native)
            .await
            .is_err()
    );
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
}

// Owned temp directories keep every record synthetic and prevent live graph access.
fn tempfile_dir() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("privacy-dispatch-{}", ulid::Ulid::new()));
    std::fs::create_dir(&path).unwrap();
    path
}

#[tokio::test]
async fn all_registry_fallback_candidates_are_decorated_after_local_timeout() {
    let dir = tempfile_dir();
    let task = task();
    let authority = setup(&dir, task.clone());
    let local = Arc::new(MockProvider {
        calls: AtomicUsize::new(0),
        fail: false,
        hang: true,
    });
    let cloud = mock(false);
    let registry = guarded_registry(
        vec![
            (local.clone(), ProviderBoundary::LocalTrusted),
            (cloud.clone(), ProviderBoundary::External),
        ],
        authority,
    );
    let candidates = registry.all_supporting(&task);
    assert_eq!(candidates.len(), 2);
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(10),
            candidates[0].invoke(&task)
        )
        .await
        .is_err()
    );
    assert!(candidates[1].invoke(&task).await.is_err());
    assert!(candidates[1].invoke(&task).await.is_err());
    assert_eq!(local.calls.load(Ordering::SeqCst), 1);
    assert_eq!(cloud.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn persisted_inherited_private_policy_denies_summary_derivative_at_external_call() {
    let dir = tempfile_dir();
    let task = task();
    let original = setup(&dir, task.clone());
    let mut derivative = ResourcePolicy::private("creator".into(), "creator".into());
    derivative.private = false;
    derivative
        .external_operations
        .insert(ProcessingOperation::Inference);
    derivative.sources.push("source".into());
    original
        .store
        .insert_policy(&actor("creator"), "summary", &derivative)
        .unwrap();
    let authority = Arc::new(FixtureAuthority {
        store: original.store.clone(),
        registered: Mutex::new(Some(task.clone())),
        sources: vec!["summary".into()],
        expected_revision: None,
    });
    let inner = mock(false);
    assert!(
        PrivacyBoundProvider::new(inner.clone(), authority, ProviderBoundary::External)
            .invoke(&task)
            .await
            .is_err()
    );
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn explicit_nonprivate_inference_permission_can_reach_mock_provider() {
    let dir = tempfile_dir();
    let task = task();
    let original = setup(&dir, task.clone());
    let mut permitted = ResourcePolicy::private("owner".into(), "creator".into());
    permitted.private = false;
    permitted
        .external_operations
        .insert(ProcessingOperation::Inference);
    original
        .store
        .insert_policy(&actor("owner"), "permitted", &permitted)
        .unwrap();
    let authority = Arc::new(FixtureAuthority {
        store: original.store.clone(),
        registered: Mutex::new(Some(task.clone())),
        sources: vec!["permitted".into()],
        expected_revision: None,
    });
    let inner = mock(false);
    assert!(matches!(
        PrivacyBoundProvider::new(inner.clone(), authority, ProviderBoundary::External)
            .invoke(&task)
            .await
            .unwrap(),
        ProviderOutput::Text { .. }
    ));
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn caller_asserted_identity_egress_and_revision_cannot_relax_registered_private_policy() {
    let dir = tempfile_dir();
    let registered = task();
    let authority = setup(&dir, registered.clone());
    let mut payload = task_payload();
    payload["agent_id"] = json!("owner");
    payload["private"] = json!(false);
    payload["egress"] = json!("allow");
    payload["policy_revision"] = json!(999999);
    let parsed = ControllerTask::from_value(&payload).unwrap();
    assert_eq!(registered, parsed); // Unknown assertions never authenticate.
    let inner = mock(false);
    assert!(
        PrivacyBoundProvider::new(inner.clone(), authority, ProviderBoundary::External)
            .invoke(&parsed)
            .await
            .is_err()
    );
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
}
