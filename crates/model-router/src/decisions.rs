//! Dispatch for `decisions.evaluate` tasks.
//!
//! A decision is not a turn reply. Every other model task answers through the
//! `model_response` action, and philote's turn handler calls `fail_active_turn`
//! on any error carried by one (`turn_loop.rs`). A decision that errors must
//! never fail a user's turn, so decisions are recognised by their raw `kind`
//! before *any* generic handling (task parsing, config load, the fallback
//! ladder) and answered on their own action, [`REPLY_ACTION`].
//!
//! This module is I/O-free apart from the provider call: it evaluates a task
//! and builds the reply body. The runtime supplies routing fields and sends it.

use crate::controller::{ControllerTask, ProviderOutput, ProviderRegistry};
use ansible_mesh_core::decisions::{
    CAPABILITY_DECISIONS_EVALUATE, DecisionsError, DecisionsErrorClass, DecisionsOutcome,
};
use serde_json::{Map, Value, json};
use std::time::{Duration, Instant};

/// The dedicated reply action. Never `model_response`.
pub const REPLY_ACTION: &str = "decisions_response";

/// Whole-call budget when the task carries no `deadline_ms`.
const DEFAULT_DEADLINE: Duration = Duration::from_millis(12_000);
const MIN_DEADLINE: Duration = Duration::from_millis(250);
const MAX_DEADLINE: Duration = Duration::from_secs(30);
/// Do not start an attempt with less than this left.
const MIN_ATTEMPT: Duration = Duration::from_millis(250);

/// True when the raw inbound task is a decisions call. Checked on the raw JSON
/// so that even an unparseable decisions task gets a decisions reply, not a
/// generic `model_response` failure.
pub fn is_decisions_task(task: &Value) -> bool {
    task.get("kind")
        .or_else(|| task.get("action"))
        .and_then(Value::as_str)
        == Some(CAPABILITY_DECISIONS_EVALUATE)
}

/// The result of one decisions call: a typed outcome or a typed error, plus
/// enough context to reply and to trace.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionReply {
    pub site: Option<String>,
    pub outcome: Result<DecisionsOutcome, DecisionsError>,
    pub latency_ms: u64,
}

impl DecisionReply {
    pub fn failed(task: &Value, class: DecisionsErrorClass, message: impl Into<String>) -> Self {
        Self {
            site: site_of(task),
            outcome: Err(DecisionsError::new(class, message)),
            latency_ms: 0,
        }
    }

    /// Failure code for the router trace store (`None` on success).
    pub fn failure_code(&self) -> Option<&'static str> {
        self.outcome.as_ref().err().map(|e| e.class.as_str())
    }

    /// Resolved model and total tokens, for the router trace store.
    pub fn model_and_tokens(&self) -> (Option<String>, Option<u64>) {
        match &self.outcome {
            Ok(o) => (
                Some(o.trace.model.clone()),
                Some(o.trace.usage.input_tokens + o.trace.usage.output_tokens),
            ),
            Err(_) => (None, None),
        }
    }

    /// Build the reply body. Routing fields (`return_route`, `session_id`, …)
    /// are added by the runtime. There is deliberately no `agent_action`, no
    /// `content` and no top-level `error`: nothing here can be read as a turn
    /// reply or a turn failure.
    pub fn body(&self, correlation_id: &str) -> Map<String, Value> {
        let decision = match &self.outcome {
            Ok(outcome) => json!({
                "status": "ok",
                "result": outcome.result,
                "trace": outcome.trace,
            }),
            Err(error) => json!({
                "status": "error",
                "error": { "class": error.class, "message": error.message },
            }),
        };
        let mut body = Map::new();
        body.insert("action".into(), json!(REPLY_ACTION));
        body.insert("capability".into(), json!(CAPABILITY_DECISIONS_EVALUATE));
        body.insert("correlation_id".into(), json!(correlation_id));
        body.insert("site".into(), json!(self.site));
        body.insert("decision".into(), decision);
        body
    }
}

fn site_of(task: &Value) -> Option<String> {
    task.get("decisions")
        .and_then(|d| d.get("site"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The caller's `correlation_id`, else the transport task id.
pub fn correlation_id(task: &Value, task_id: &str) -> String {
    task.get("correlation_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(task_id)
        .to_string()
}

fn deadline_of(task: &Value) -> Duration {
    task.get("deadline_ms")
        .and_then(Value::as_u64)
        .map(Duration::from_millis)
        .map_or(DEFAULT_DEADLINE, |d| d.clamp(MIN_DEADLINE, MAX_DEADLINE))
}

/// Evaluate a decisions task against `providers`.
///
/// Never panics and never returns a bare `anyhow` error: every path ends in a
/// typed [`DecisionsError`] whose meaning is "use the deterministic decision".
/// Timeouts and unavailability get one retry inside the deadline; rate limits
/// are not retried (hammering a 429 or 529 only makes it worse).
pub async fn evaluate_task(task_value: &Value, providers: &ProviderRegistry) -> DecisionReply {
    let started = Instant::now();
    let site = site_of(task_value);
    let finish = |outcome: Result<DecisionsOutcome, DecisionsError>| DecisionReply {
        site: site.clone(),
        outcome,
        latency_ms: started.elapsed().as_millis() as u64,
    };

    let task = match ControllerTask::from_value(task_value) {
        Ok(task) => task,
        Err(err) => {
            return finish(Err(DecisionsError::new(
                DecisionsErrorClass::InvalidRequest,
                format!("could not interpret decisions task: {err:#}"),
            )));
        }
    };
    let provider = match providers.resolve(&task) {
        Ok(provider) => provider,
        Err(err) => {
            return finish(Err(DecisionsError::new(
                DecisionsErrorClass::Unavailable,
                format!("no decisions provider: {err:#}"),
            )));
        }
    };

    let deadline = deadline_of(task_value);
    let attempt_cap = Duration::from_secs(provider.attempt_policy().total_secs);
    let max_attempts = provider.retry_policy().max_attempts.max(1);
    let backoff = match provider.retry_policy().backoff {
        crate::controller::BackoffStrategy::None => Duration::ZERO,
        crate::controller::BackoffStrategy::Linear { step_ms } => Duration::from_millis(step_ms),
    };

    let mut attempt = 0u8;
    loop {
        attempt += 1;
        let remaining = deadline.saturating_sub(started.elapsed());
        if remaining < MIN_ATTEMPT {
            return finish(Err(DecisionsError::new(
                DecisionsErrorClass::Timeout,
                "deadline exhausted before the provider answered",
            )));
        }
        let this_attempt = remaining.min(attempt_cap);
        let result = match tokio::time::timeout(this_attempt, provider.invoke(&task)).await {
            Ok(Ok(ProviderOutput::Judgment(outcome))) => Ok(*outcome),
            Ok(Ok(_)) => Err(DecisionsError::new(
                DecisionsErrorClass::InvalidResponse,
                "decisions provider returned a non-judgment output",
            )),
            // The typed class survives inside the anyhow error; anything else
            // is an untyped provider failure and reads as unavailable.
            Ok(Err(err)) => Err(err.downcast::<DecisionsError>().unwrap_or_else(|other| {
                DecisionsError::new(DecisionsErrorClass::Unavailable, format!("{other:#}"))
            })),
            Err(_) => Err(DecisionsError::new(
                DecisionsErrorClass::Timeout,
                format!("no answer within {} ms", this_attempt.as_millis()),
            )),
        };
        match result {
            Err(err)
                if attempt < max_attempts
                    && matches!(
                        err.class,
                        DecisionsErrorClass::Unavailable | DecisionsErrorClass::Timeout
                    ) =>
            {
                tokio::time::sleep(backoff).await;
            }
            other => return finish(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{AttemptPolicy, ModelProvider, RetryPolicy, TaskKind};
    use ansible_mesh_core::decisions::{
        DecisionAnswer, DecisionQuestion, DecisionsRequest, DecisionsResult, DecisionsTrace,
        DecisionsTransport, DecisionsUsage, QuestionSpec,
    };
    use async_trait::async_trait;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn request() -> DecisionsRequest {
        DecisionsRequest {
            site: "heal.classify".into(),
            state: json!("connection refused"),
            questions: vec![DecisionQuestion {
                id: "needs_restart".into(),
                instructions: "Would a restart fix this?".into(),
                spec: QuestionSpec::Noul {
                    when_true: None,
                    when_false: None,
                },
            }],
        }
    }

    fn task_value() -> Value {
        json!({
            "kind": "decisions.evaluate",
            "request_class": "judgment",
            "session_id": "s1",
            "turn_id": "t1",
            "decisions": serde_json::to_value(request()).unwrap(),
        })
    }

    fn outcome() -> DecisionsOutcome {
        DecisionsOutcome {
            result: DecisionsResult {
                site: "heal.classify".into(),
                answers: BTreeMap::from([(
                    "needs_restart".to_string(),
                    DecisionAnswer::Noul { noul: 0.07 },
                )]),
            },
            trace: DecisionsTrace {
                provider: "TypeSafe".into(),
                model: "typesafe/jev-1.13-20260917".into(),
                transport: DecisionsTransport::OpenRouter,
                latency_ms: 290,
                usage: DecisionsUsage {
                    input_tokens: 307,
                    output_tokens: 23,
                    cost_usd: Some(0.0000129),
                },
                request_id: Some("gen-dec-x".into()),
                legend_mismatch: false,
            },
        }
    }

    /// A provider that plays back a scripted sequence of results.
    struct Scripted {
        calls: AtomicUsize,
        script: Vec<Result<ProviderOutput, DecisionsError>>,
        hang: bool,
    }

    impl Scripted {
        fn new(script: Vec<Result<ProviderOutput, DecisionsError>>) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                script,
                hang: false,
            })
        }
    }

    #[async_trait]
    impl ModelProvider for Scripted {
        fn id(&self) -> &'static str {
            "typesafe"
        }
        fn supports(&self, task: &ControllerTask) -> bool {
            task.kind == TaskKind::Decide
        }
        async fn invoke(&self, _task: &ControllerTask) -> anyhow::Result<ProviderOutput> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if self.hang {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
            match self
                .script
                .get(call)
                .or(self.script.last())
                .expect("script")
            {
                Ok(output) => Ok(output.clone()),
                Err(err) => Err(anyhow::Error::from(err.clone())),
            }
        }
        fn attempt_policy(&self) -> AttemptPolicy {
            AttemptPolicy {
                connect_secs: 1,
                idle_secs: 1,
                total_secs: 1,
            }
        }
        fn retry_policy(&self) -> RetryPolicy {
            RetryPolicy {
                max_attempts: 2,
                backoff: crate::controller::BackoffStrategy::None,
                retryable: Default::default(),
            }
        }
    }

    fn registry(provider: Arc<Scripted>) -> ProviderRegistry {
        ProviderRegistry::new(vec![provider])
    }

    fn err(class: DecisionsErrorClass) -> Result<ProviderOutput, DecisionsError> {
        Err(DecisionsError::new(class, "scripted"))
    }

    #[test]
    fn decisions_tasks_are_recognised_from_the_raw_kind() {
        assert!(is_decisions_task(&json!({ "kind": "decisions.evaluate" })));
        assert!(is_decisions_task(
            &json!({ "action": "decisions.evaluate" })
        ));
        assert!(!is_decisions_task(&json!({ "kind": "text.generate" })));
        assert!(!is_decisions_task(&json!({ "kind": "text.embed" })));
        assert!(!is_decisions_task(&json!({})));
        // Even a decisions task with a broken envelope is still a decisions task.
        assert!(is_decisions_task(
            &json!({ "kind": "decisions.evaluate", "decisions": 7 })
        ));
    }

    #[test]
    fn the_reply_can_never_be_read_as_a_turn_reply_or_a_turn_failure() {
        let ok = DecisionReply {
            site: Some("heal.classify".into()),
            outcome: Ok(outcome()),
            latency_ms: 1,
        };
        let failed =
            DecisionReply::failed(&task_value(), DecisionsErrorClass::RateLimited, "slow down");
        for reply in [ok, failed] {
            let body = reply.body("corr-1");
            assert_eq!(body["action"], REPLY_ACTION);
            assert_ne!(body["action"], "model_response");
            for forbidden in ["agent_action", "content", "error", "model_result"] {
                assert!(
                    !body.contains_key(forbidden),
                    "`{forbidden}` at the top level could be read as a turn reply or failure"
                );
            }
            assert_eq!(body["correlation_id"], "corr-1");
            assert_eq!(body["capability"], "decisions.evaluate");
            assert_eq!(body["site"], "heal.classify");
        }
    }

    #[test]
    fn ok_and_error_bodies_carry_typed_payloads() {
        let ok = DecisionReply {
            site: Some("heal.classify".into()),
            outcome: Ok(outcome()),
            latency_ms: 1,
        }
        .body("c");
        assert_eq!(ok["decision"]["status"], "ok");
        assert_eq!(
            ok["decision"]["result"]["answers"]["needs_restart"]["noul"],
            0.07
        );
        assert_eq!(
            ok["decision"]["trace"]["model"],
            "typesafe/jev-1.13-20260917"
        );
        assert_eq!(ok["decision"]["trace"]["usage"]["output_tokens"], 23);

        let failed =
            DecisionReply::failed(&task_value(), DecisionsErrorClass::Auth, "no key").body("c");
        assert_eq!(failed["decision"]["status"], "error");
        assert_eq!(failed["decision"]["error"]["class"], "auth");
        assert_eq!(failed["site"], "heal.classify");
    }

    #[test]
    fn correlation_falls_back_to_the_task_id() {
        assert_eq!(
            correlation_id(&json!({ "correlation_id": "mine" }), "task-9"),
            "mine"
        );
        assert_eq!(correlation_id(&json!({}), "task-9"), "task-9");
        assert_eq!(
            correlation_id(&json!({ "correlation_id": "" }), "task-9"),
            "task-9"
        );
    }

    #[test]
    fn deadline_is_clamped_and_defaulted() {
        assert_eq!(deadline_of(&json!({})), DEFAULT_DEADLINE);
        assert_eq!(deadline_of(&json!({ "deadline_ms": 1 })), MIN_DEADLINE);
        assert_eq!(
            deadline_of(&json!({ "deadline_ms": 99_999_999 })),
            MAX_DEADLINE
        );
        assert_eq!(
            deadline_of(&json!({ "deadline_ms": 1500 })),
            Duration::from_millis(1500)
        );
    }

    #[tokio::test]
    async fn a_successful_call_returns_the_typed_outcome() {
        let provider = Scripted::new(vec![Ok(ProviderOutput::Judgment(Box::new(outcome())))]);
        let reply = evaluate_task(&task_value(), &registry(provider.clone())).await;
        assert_eq!(reply.outcome.unwrap(), outcome());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(reply.site.as_deref(), Some("heal.classify"));
    }

    #[tokio::test]
    async fn unavailable_is_retried_once_then_succeeds() {
        let provider = Scripted::new(vec![
            err(DecisionsErrorClass::Unavailable),
            Ok(ProviderOutput::Judgment(Box::new(outcome()))),
        ]);
        let reply = evaluate_task(&task_value(), &registry(provider.clone())).await;
        assert!(reply.outcome.is_ok());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn rate_limits_and_auth_failures_are_not_retried() {
        for class in [
            DecisionsErrorClass::RateLimited,
            DecisionsErrorClass::Auth,
            DecisionsErrorClass::InvalidRequest,
            DecisionsErrorClass::InvalidResponse,
        ] {
            let provider = Scripted::new(vec![err(class)]);
            let reply = evaluate_task(&task_value(), &registry(provider.clone())).await;
            assert_eq!(reply.outcome.unwrap_err().class, class);
            assert_eq!(provider.calls.load(Ordering::SeqCst), 1, "{class}");
        }
    }

    #[tokio::test]
    async fn retries_stop_at_the_attempt_budget_and_keep_the_last_class() {
        let provider = Scripted::new(vec![err(DecisionsErrorClass::Unavailable)]);
        let reply = evaluate_task(&task_value(), &registry(provider.clone())).await;
        assert_eq!(
            reply.outcome.unwrap_err().class,
            DecisionsErrorClass::Unavailable
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_hung_provider_times_out_inside_the_deadline() {
        let provider = Arc::new(Scripted {
            calls: AtomicUsize::new(0),
            script: vec![err(DecisionsErrorClass::Unavailable)],
            hang: true,
        });
        let mut task = task_value();
        // The 250 ms floor is the shortest deadline a caller can ask for.
        task["deadline_ms"] = json!(300);
        let reply = evaluate_task(&task, &registry(provider)).await;
        assert_eq!(
            reply.outcome.unwrap_err().class,
            DecisionsErrorClass::Timeout
        );
    }

    #[tokio::test]
    async fn a_malformed_envelope_and_a_missing_provider_are_typed_errors_not_panics() {
        let provider = Scripted::new(vec![Ok(ProviderOutput::Judgment(Box::new(outcome())))]);

        let mut bad = task_value();
        bad["decisions"] = json!({ "site": "heal.classify", "state": "x", "questions": [] });
        let reply = evaluate_task(&bad, &registry(provider.clone())).await;
        assert_eq!(
            reply.outcome.unwrap_err().class,
            DecisionsErrorClass::InvalidRequest
        );
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            0,
            "rejected before any provider hop"
        );

        let mut junk = task_value();
        junk["decisions"] = json!(7);
        let reply = evaluate_task(&junk, &registry(provider)).await;
        assert_eq!(
            reply.outcome.unwrap_err().class,
            DecisionsErrorClass::InvalidRequest
        );

        let none = ProviderRegistry::new(Vec::new());
        let reply = evaluate_task(&task_value(), &none).await;
        assert_eq!(
            reply.outcome.unwrap_err().class,
            DecisionsErrorClass::Unavailable
        );
    }

    #[tokio::test]
    async fn a_non_judgment_output_is_an_invalid_response() {
        let provider = Scripted::new(vec![Ok(ProviderOutput::Embedding {
            vector: vec![0.1],
            model_gen: "m@1".into(),
        })]);
        let reply = evaluate_task(&task_value(), &registry(provider)).await;
        assert_eq!(
            reply.outcome.unwrap_err().class,
            DecisionsErrorClass::InvalidResponse
        );
    }
}
