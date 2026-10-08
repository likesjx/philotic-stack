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
