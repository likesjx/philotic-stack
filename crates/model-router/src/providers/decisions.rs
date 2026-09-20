//! Decisions provider: typed judgments from TypeSafe's Jev, native or via
//! OpenRouter's alpha endpoint. Neither is chat completions, which is why the
//! existing OpenAI-compatible provider cannot serve it.
//!
//! This is a thin HTTP hop around the pure adapters in
//! `ansible_mesh_core::decisions`; all validation, ordering and response
//! checking lives there. Every failure is a typed `DecisionsError` meaning
//! "use the deterministic decision", and `evaluate` never panics on bad input.

use crate::controller::{
    AttemptPolicy, BackoffStrategy, ControllerTask, ModelProvider, ProviderOutput, RetryPolicy,
    RetryableErrorClass, TaskKind,
};
use ansible_mesh_core::decisions::{
    DecisionsError, DecisionsErrorClass, DecisionsOutcome, DecisionsRequest, DecisionsTransport,
    PINNED_OPENROUTER_MODEL, build_wire_request, classify_http_status, parse_wire_response,
};
use anyhow::Result;
use async_trait::async_trait;
use std::time::{Duration, Instant};

/// Provider id. The transport is an internal detail of this provider, chosen
/// by which credential is configured, so callers name `typesafe`, never a route.
pub const PROVIDER_ID: &str = "typesafe";

const NATIVE_BASE_URL: &str = "https://api.typesafe.ai";
const NATIVE_DEFAULT_MODEL: &str = "jev-latest";
/// Decisions are small and off the model ladder: single-digit seconds, one retry.
const ATTEMPT_TOTAL_SECS: u64 = 8;

pub struct DecisionsProvider {
    http: reqwest::Client,
    transport: DecisionsTransport,
    base_url: String,
    api_key: Option<String>,
    default_model: String,
}

impl DecisionsProvider {
    /// OpenRouter alpha transport. `base_url` is the hotel's OpenRouter base
    /// (`https://openrouter.ai/api`); the decisions path already begins with
    /// `/api`, so that suffix is dropped rather than doubled.
    pub fn openrouter(
        http: reqwest::Client,
        api_key: Option<String>,
        base_url: Option<String>,
        default_model: Option<String>,
    ) -> Self {
        let base = base_url
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .unwrap_or("https://openrouter.ai")
            .trim_end_matches('/');
        let base = base.strip_suffix("/api").unwrap_or(base);
        Self {
            http,
            transport: DecisionsTransport::OpenRouter,
            base_url: base.to_string(),
            api_key: clean(api_key),
            // Pinned, never the moving alias: calibration is per model version.
            default_model: default_model.unwrap_or_else(|| PINNED_OPENROUTER_MODEL.to_string()),
        }
    }

    /// Native TypeSafe transport (early access, bearer key).
    pub fn native(
        http: reqwest::Client,
        api_key: Option<String>,
        base_url: Option<String>,
    ) -> Self {
        Self {
            http,
            transport: DecisionsTransport::Native,
            base_url: base_url
                .as_deref()
                .map(str::trim)
                .filter(|b| !b.is_empty())
                .unwrap_or(NATIVE_BASE_URL)
                .trim_end_matches('/')
                .to_string(),
            api_key: clean(api_key),
            default_model: NATIVE_DEFAULT_MODEL.to_string(),
        }
    }

    pub fn transport(&self) -> DecisionsTransport {
        self.transport
    }

    fn url(&self) -> String {
        format!("{}{}", self.base_url, self.transport.path())
    }

    /// One provider hop. `model` overrides the provider default; either may be
    /// a provider-neutral name or an already-resolved wire slug.
    pub async fn evaluate(
        &self,
        request: &DecisionsRequest,
        model: Option<&str>,
        timeout: Duration,
    ) -> Result<DecisionsOutcome, DecisionsError> {
        let Some(key) = self.api_key.as_deref() else {
            return Err(DecisionsError::new(
                DecisionsErrorClass::Auth,
                format!(
                    "no API key configured for the {} transport",
                    self.transport.as_str()
                ),
            ));
        };
        let body = build_wire_request(
            self.transport,
            request,
            model.unwrap_or(self.default_model.as_str()),
        )?;

        let started = Instant::now();
        let response = self
            .http
            .post(self.url())
            .bearer_auth(key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .timeout(timeout)
            .body(body)
            .send()
            .await
            .map_err(|err| {
                let class = if err.is_timeout() {
                    DecisionsErrorClass::Timeout
                } else {
                    DecisionsErrorClass::Unavailable
                };
                // `without_url` keeps request URLs out of logs and heal entries.
                DecisionsError::new(class, err.without_url().to_string())
            })?;

        let status = response.status();
        let text = response.text().await.map_err(|err| {
            DecisionsError::new(
                DecisionsErrorClass::Unavailable,
                err.without_url().to_string(),
            )
        })?;
        let latency_ms = started.elapsed().as_millis() as u64;
        if !status.is_success() {
            return Err(classify_http_status(status.as_u16(), &text));
        }
        Ok(parse_wire_response(request, &text)?.into_outcome(self.transport, latency_ms))
    }
}

fn clean(key: Option<String>) -> Option<String> {
    key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
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
            .evaluate(
                request,
                task.model.as_deref(),
                Duration::from_secs(ATTEMPT_TOTAL_SECS),
            )
            .await
            .map_err(anyhow::Error::from)?;
        Ok(ProviderOutput::Judgment(Box::new(outcome)))
    }

    fn attempt_policy(&self) -> AttemptPolicy {
        AttemptPolicy {
            connect_secs: 3,
            idle_secs: ATTEMPT_TOTAL_SECS,
            total_secs: ATTEMPT_TOTAL_SECS,
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
        DecisionAnswer, DecisionOption, DecisionQuestion, QuestionSpec,
    };
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn request() -> DecisionsRequest {
        DecisionsRequest {
            site: "smoke.urgency".into(),
            state: json!("Help! My payouts have been failing for 3 days."),
            questions: vec![
                DecisionQuestion {
                    id: "zz_urgent".into(),
                    instructions: "Does this convey urgency?".into(),
                    spec: QuestionSpec::Noul {
                        when_true: None,
                        when_false: None,
                    },
                },
                DecisionQuestion {
                    id: "aa_kind".into(),
                    instructions: "What kind of message is this?".into(),
                    spec: QuestionSpec::Choice {
                        options: vec![
                            DecisionOption::new("zeta", "a problem report"),
                            DecisionOption::new("alpha", "a question"),
                        ],
                    },
                },
            ],
        }
    }

    const OK_BODY: &str = r#"{"model":"typesafe/jev-1.13-20260917","answers":{"zz_urgent":{"type":"noul","noul":0.95},"aa_kind":{"type":"choice","choice":"zeta","probabilities":{"zeta":0.9,"alpha":0.05},"confidence":0.9}},"usage":{"input_tokens":307,"output_tokens":23,"cost":0.000012894},"id":"gen-dec-x","provider":"TypeSafe"}"#;

    /// Serve exactly one canned HTTP response on a random local port and return
    /// the base URL plus a handle yielding the raw request the server saw.
    async fn serve_once(
        status_line: &'static str,
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                seen.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&seen).to_string();
                if let Some(split) = text.find("\r\n\r\n") {
                    let length = text[..split]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if seen.len() >= split + 4 + length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            let _ = socket.shutdown().await;
            String::from_utf8_lossy(&seen).to_string()
        });
        (format!("http://{addr}"), handle)
    }

    fn provider(base: &str) -> DecisionsProvider {
        // The hotel's configured base ends in `/api`; the provider must not double it.
        DecisionsProvider::openrouter(
            reqwest::Client::new(),
            Some("test-key".into()),
            Some(format!("{base}/api")),
            None,
        )
    }

    #[test]
    fn openrouter_base_url_never_doubles_the_api_segment() {
        let http = reqwest::Client::new();
        for base in [
            "https://openrouter.ai/api",
            "https://openrouter.ai/api/",
            "https://openrouter.ai",
            "https://openrouter.ai/",
        ] {
            let p = DecisionsProvider::openrouter(http.clone(), None, Some(base.into()), None);
            assert_eq!(
                p.url(),
                "https://openrouter.ai/api/alpha/decisions",
                "{base}"
            );
        }
        let default = DecisionsProvider::openrouter(http.clone(), None, None, None);
        assert_eq!(default.url(), "https://openrouter.ai/api/alpha/decisions");
        let native = DecisionsProvider::native(http, None, None);
        assert_eq!(native.url(), "https://api.typesafe.ai/v1/systemone");
    }

    #[tokio::test]
    async fn success_sends_the_pinned_model_in_order_and_returns_typed_answers() {
        let (base, seen) = serve_once("200 OK", OK_BODY).await;
        let outcome = provider(&base)
            .evaluate(&request(), None, Duration::from_secs(5))
            .await
            .unwrap();

        let raw = seen.await.unwrap();
        assert!(
            raw.starts_with("POST /api/alpha/decisions HTTP/1.1"),
            "{raw}"
        );
        assert!(
            raw.to_ascii_lowercase()
                .contains("authorization: bearer test-key")
        );
        assert!(
            raw.contains("\"model\":\"typesafe/jev-1.13\""),
            "pinned slug, not the alias: {raw}"
        );
        assert!(
            raw.find("zz_urgent").unwrap() < raw.find("aa_kind").unwrap(),
            "question order is preserved on the wire"
        );

        assert_eq!(outcome.trace.transport, DecisionsTransport::OpenRouter);
        assert_eq!(outcome.trace.model, "typesafe/jev-1.13-20260917");
        assert_eq!(outcome.trace.provider, "TypeSafe");
        assert_eq!(outcome.trace.usage.output_tokens, 23);
        assert!(matches!(
            outcome.result.answers["zz_urgent"],
            DecisionAnswer::Noul { noul } if noul == 0.95
        ));
    }

    #[tokio::test]
    async fn http_failures_keep_their_class_and_never_leak_the_key() {
        for (line, body, class) in [
            (
                "401 Unauthorized",
                r#"{"error":{"message":"Missing Authentication header"}}"#,
                DecisionsErrorClass::Auth,
            ),
            (
                "429 Too Many Requests",
                "slow down",
                DecisionsErrorClass::RateLimited,
            ),
            (
                "400 Bad Request",
                r#"{"error":"Model does not exist"}"#,
                DecisionsErrorClass::InvalidRequest,
            ),
            (
                "503 Service Unavailable",
                "overloaded",
                DecisionsErrorClass::Unavailable,
            ),
        ] {
            let (base, _seen) = serve_once(line, body).await;
            let err = provider(&base)
                .evaluate(&request(), None, Duration::from_secs(5))
                .await
                .expect_err(line);
            assert_eq!(err.class, class, "{line}");
            assert!(!err.message.contains("test-key"));
        }
    }

    #[tokio::test]
    async fn an_unusable_answer_is_an_invalid_response_not_a_verdict() {
        // A choice the request never offered.
        let body = r#"{"model":"m","answers":{"zz_urgent":{"type":"noul","noul":0.5},"aa_kind":{"type":"choice","choice":"gamma","probabilities":{},"confidence":0.5}}}"#;
        let (base, _seen) = serve_once("200 OK", body).await;
        let err = provider(&base)
            .evaluate(&request(), None, Duration::from_secs(5))
            .await
            .expect_err("unknown choice");
        assert_eq!(err.class, DecisionsErrorClass::InvalidResponse);
    }

    #[tokio::test]
    async fn a_missing_key_fails_as_auth_without_touching_the_network() {
        let p =
            DecisionsProvider::openrouter(reqwest::Client::new(), Some("  ".into()), None, None);
        let err = p
            .evaluate(&request(), None, Duration::from_secs(5))
            .await
            .expect_err("no key");
        assert_eq!(err.class, DecisionsErrorClass::Auth);
    }

    #[tokio::test]
    async fn an_invalid_request_is_rejected_before_any_request_is_sent() {
        let mut bad = request();
        bad.questions.clear();
        // Nothing listens on this port; an attempted connection would be `Unavailable`.
        let p = DecisionsProvider::openrouter(
            reqwest::Client::new(),
            Some("k".into()),
            Some("http://127.0.0.1:9".into()),
            None,
        );
        let err = p
            .evaluate(&bad, None, Duration::from_secs(1))
            .await
            .expect_err("invalid");
        assert_eq!(err.class, DecisionsErrorClass::InvalidRequest);
    }

    #[tokio::test]
    async fn an_unreachable_provider_is_unavailable() {
        let p = DecisionsProvider::openrouter(
            reqwest::Client::new(),
            Some("k".into()),
            Some("http://127.0.0.1:9".into()),
            None,
        );
        let err = p
            .evaluate(&request(), None, Duration::from_secs(2))
            .await
            .expect_err("nothing listening");
        assert_eq!(err.class, DecisionsErrorClass::Unavailable);
    }

    #[tokio::test]
    async fn invoke_wraps_the_outcome_and_preserves_the_error_class_through_anyhow() {
        let p = DecisionsProvider::openrouter(reqwest::Client::new(), None, None, None);
        let task = ControllerTask::from_value(&json!({
            "kind": "decisions.evaluate",
            "decisions": serde_json::to_value(request()).unwrap(),
        }))
        .unwrap();
        assert!(p.supports(&task));
        let err = p.invoke(&task).await.expect_err("no key");
        let typed = err
            .downcast_ref::<DecisionsError>()
            .expect("typed error survives");
        assert_eq!(typed.class, DecisionsErrorClass::Auth);
    }
}
