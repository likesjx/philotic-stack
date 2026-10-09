//! Final invocation decorator. Hotel/IPC installation is pending coordination.
//! All retries and fallback candidates must use decorated providers. Native-live
//! sessions are a separate boundary and are not secured by this decorator.

use crate::controller::{
    AttemptPolicy, ControllerTask, ModelProvider, ProviderOutput, ProviderRegistry, RetryPolicy,
    TaskKind,
};
use ansible_mesh_core::privacy::{
    AuthenticatedAgent, ProcessingOperation, ProviderBoundary, authorize_processing,
};
use ansible_mesh_core::privacy_storage::PolicySnapshot;
use anyhow::{Result, bail};
use async_trait::async_trait;
use std::sync::Arc;

pub struct VerifiedDispatchContext {
    pub actor: AuthenticatedAgent,
    pub policies: PolicySnapshot,
    pub sources: Vec<String>,
}

/// Implemented only by the authenticated server-session/manifest owner. Must
/// bind the ENTIRE ControllerTask (prompt, summaries, tools, tool results,
/// attachments and context) to a complete immutable manifest and current policy
/// snapshot. JSON agent/private/egress fields never establish this context.
/// Unknown/stale/mismatched context returns None, never an empty permissive map.
/// This contract has no production IPC adapter yet.
pub trait DispatchPrivacyAuthority: Send + Sync {
    fn context_for(&self, task: &ControllerTask) -> Option<VerifiedDispatchContext>;
}

pub struct PrivacyBoundProvider {
    inner: Arc<dyn ModelProvider>,
    authority: Arc<dyn DispatchPrivacyAuthority>,
    boundary: ProviderBoundary,
}

/// Constructs the existing registry with every selectable/fallback candidate
/// decorated. No bare provider escapes through resolve() or all_supporting().
/// The hotel still needs to install this factory with its authentic authority.
pub fn guarded_registry(
    providers: Vec<(Arc<dyn ModelProvider>, ProviderBoundary)>,
    authority: Arc<dyn DispatchPrivacyAuthority>,
) -> ProviderRegistry {
    ProviderRegistry::new(
        providers
            .into_iter()
            .map(|(provider, boundary)| {
                Arc::new(PrivacyBoundProvider::new(
                    provider,
                    authority.clone(),
                    boundary,
                )) as Arc<dyn ModelProvider>
            })
            .collect(),
    )
}

impl PrivacyBoundProvider {
    /// Endpoint classification is resolved by the server; a local cloud proxy
    /// is External. Never derive this from provider ID or caller hints.
    pub fn new(
        inner: Arc<dyn ModelProvider>,
        authority: Arc<dyn DispatchPrivacyAuthority>,
        boundary: ProviderBoundary,
    ) -> Self {
        Self {
            inner,
            authority,
            boundary,
        }
    }

    fn check(&self, task: &ControllerTask) -> Result<()> {
        let Some(context) = self.authority.context_for(task) else {
            bail!("privacy_denied: missing authoritative request context");
        };
        let operation = match task.kind {
            TaskKind::TextGenerate | TaskKind::MediaAnalyze | TaskKind::Decide => {
                ProcessingOperation::Inference
            }
            TaskKind::Embed => ProcessingOperation::Embedding,
            TaskKind::AudioTranscribe => ProcessingOperation::SpeechToText,
            TaskKind::VoiceSynthesize => ProcessingOperation::TextToSpeech,
            TaskKind::VoiceDialogue | TaskKind::ResponseGenerate => {
                bail!("privacy_denied: native-live path requires separate enforcement")
            }
        };
        authorize_processing(
            &context.policies,
            Some(&context.actor),
            &context.sources,
            operation,
            self.boundary,
        )
        .map_err(|denial| anyhow::anyhow!("privacy_denied: {denial:?}"))
    }
}

#[async_trait]
impl ModelProvider for PrivacyBoundProvider {
    fn id(&self) -> &'static str {
        self.inner.id()
    }
    fn supports(&self, task: &ControllerTask) -> bool {
        !task.kind.is_native_live() && self.inner.supports(task)
    }
    fn supports_streaming(&self, task: &ControllerTask) -> bool {
        !task.kind.is_native_live() && self.inner.supports_streaming(task)
    }
    fn context_models(&self, task: &ControllerTask) -> Vec<String> {
        self.inner.context_models(task)
    }
    fn attempt_policy(&self) -> AttemptPolicy {
        self.inner.attempt_policy()
    }
    fn retry_policy(&self) -> RetryPolicy {
        self.inner.retry_policy()
    }
    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        self.check(task)?;
        self.inner.invoke(task).await
    }
    async fn invoke_streaming(
        &self,
        task: &ControllerTask,
        tokens: tokio::sync::mpsc::Sender<String>,
    ) -> Result<ProviderOutput> {
        self.check(task)?;
        self.inner.invoke_streaming(task, tokens).await
    }
}

#[cfg(test)]
mod context_candidate_tests {
    use super::*;
    use serde_json::json;

    struct Candidates;
    #[async_trait]
    impl ModelProvider for Candidates {
        fn id(&self) -> &'static str {
            "openrouter"
        }
        fn supports(&self, _: &ControllerTask) -> bool {
            true
        }
        fn context_models(&self, task: &ControllerTask) -> Vec<String> {
            task.model
                .clone()
                .map(|model| vec![model])
                .unwrap_or_else(|| vec!["synthetic/default".into(), "synthetic/fallback".into()])
        }
        async fn invoke(&self, _: &ControllerTask) -> Result<ProviderOutput> {
            bail!("synthetic provider must not be called")
        }
    }
    struct Deny;
    impl DispatchPrivacyAuthority for Deny {
        fn context_for(&self, _: &ControllerTask) -> Option<VerifiedDispatchContext> {
            None
        }
    }

    #[test]
    fn guarded_provider_preserves_default_fallback_and_unknown_pinned_candidates() {
        let guarded = PrivacyBoundProvider::new(
            Arc::new(Candidates),
            Arc::new(Deny),
            ProviderBoundary::Unknown,
        );
        let catalog = crate::context_management::parse_openrouter_capabilities(
            r#"[{"id":"synthetic/default","ctx":100000},{"id":"synthetic/fallback","ctx":8000}]"#,
        );
        let mut task =
            ControllerTask::from_value(&json!({"kind":"text.generate", "prompt":"synthetic"}))
                .unwrap();
        assert_eq!(
            guarded.context_models(&task),
            vec!["synthetic/default", "synthetic/fallback"]
        );
        task.resolved_context_capabilities = crate::context_management::resolve_capabilities(
            guarded.id(),
            guarded.context_models(&task),
            &catalog,
        );
        assert_eq!(crate::context_management::limits(&task).unwrap().1, 8000);
        task.model = Some("synthetic/unknown-pinned".into());
        assert_eq!(
            guarded.context_models(&task),
            vec!["synthetic/unknown-pinned"]
        );
        task.resolved_context_capabilities = crate::context_management::resolve_capabilities(
            guarded.id(),
            guarded.context_models(&task),
            &catalog,
        );
        assert_eq!(crate::context_management::limits(&task).unwrap().1, 16_384);
        assert!(
            guarded.check(&task).is_err(),
            "candidate metadata grants no authority"
        );
    }
}
