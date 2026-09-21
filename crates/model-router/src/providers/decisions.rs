//! `ModelProvider` wrapper around `decisions_client::DecisionsClient`, which owns
//! the HTTP hop (native TypeSafe or OpenRouter's alpha decisions endpoint) and the
//! typed errors. This file only adapts it to the router's provider trait.

use crate::controller::{
    AttemptPolicy, BackoffStrategy, ControllerTask, ModelProvider, ProviderOutput, RetryPolicy,
    RetryableErrorClass, TaskKind,
};
use anyhow::Result;
use async_trait::async_trait;
use decisions_client::{DEFAULT_ATTEMPT_SECS, DecisionsClient};
use std::time::Duration;

/// Provider id. The transport is an internal detail, chosen by which credential
/// is configured, so callers name `typesafe`, never a route.
pub const PROVIDER_ID: &str = "typesafe";

pub struct DecisionsProvider {
    client: DecisionsClient,
}

impl DecisionsProvider {
    pub fn new(client: DecisionsClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl ModelProvider for DecisionsProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn supports(&self, task: &ControllerTask) -> bool {
        task.kind == TaskKind::Decide && task.decisions.is_some()
    }

    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        let request = task
            .decisions
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("decisions.evaluate task has no `decisions` block"))?;
        // The typed error is preserved inside the anyhow error so the decisions
        // handler can downcast it and keep the class.
        let outcome = self
            .client
            .evaluate(
                request,
                task.model.as_deref(),
                Duration::from_secs(DEFAULT_ATTEMPT_SECS),
            )
            .await
            .map_err(anyhow::Error::from)?;
        Ok(ProviderOutput::Judgment(Box::new(outcome)))
    }

    fn attempt_policy(&self) -> AttemptPolicy {
        AttemptPolicy {
            connect_secs: 3,
            idle_secs: DEFAULT_ATTEMPT_SECS,
            total_secs: DEFAULT_ATTEMPT_SECS,
        }
    }

    /// One retry, tight backoff. The decisions handler applies its own class-based
    /// retry (never on 429/529) and always ends in a typed error reply.
    fn retry_policy(&self) -> RetryPolicy {
        RetryPolicy {
            max_attempts: 2,
            backoff: BackoffStrategy::Linear { step_ms: 250 },
            retryable: RetryableErrorClass::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::decisions::{
        DecisionOption, DecisionQuestion, DecisionsError, DecisionsErrorClass, DecisionsRequest,
        QuestionSpec,
    };
    use serde_json::json;

    fn task() -> ControllerTask {
        let request = DecisionsRequest {
            site: "smoke.urgency".into(),
            state: json!("Help! My payouts have been failing for 3 days."),
            questions: vec![DecisionQuestion {
                id: "kind".into(),
                instructions: "What kind of message is this?".into(),
                spec: QuestionSpec::Choice {
                    options: vec![
                        DecisionOption::new("problem", "a problem report"),
                        DecisionOption::new("question", "a question"),
                    ],
                },
            }],
        };
        ControllerTask::from_value(&json!({
            "kind": "decisions.evaluate",
            "decisions": serde_json::to_value(request).unwrap(),
        }))
        .unwrap()
    }

    #[test]
    fn supports_only_decide_tasks_with_a_block() {
        let provider = DecisionsProvider::new(DecisionsClient::openrouter(
            reqwest::Client::new(),
            None,
            None,
            None,
        ));
        assert_eq!(provider.id(), "typesafe");
        assert!(provider.supports(&task()));
        let text = ControllerTask::from_value(&json!({ "kind": "text.generate", "prompt": "hi" }))
            .unwrap();
        assert!(!provider.supports(&text));
    }

    #[tokio::test]
    async fn invoke_preserves_the_typed_error_class_through_anyhow() {
        // No key: fails as Auth before any network hop.
        let provider = DecisionsProvider::new(DecisionsClient::openrouter(
            reqwest::Client::new(),
            None,
            None,
            None,
        ));
        let err = provider.invoke(&task()).await.expect_err("no key");
        let typed = err
            .downcast_ref::<DecisionsError>()
            .expect("typed error survives");
        assert_eq!(typed.class, DecisionsErrorClass::Auth);
    }
}
