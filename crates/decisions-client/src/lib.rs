//! Client for typed decisions: TypeSafe's Jev, native or via OpenRouter's
//! alpha endpoint. Neither is chat completions, which is why the existing
//! OpenAI-compatible provider cannot serve it.
//!
//! One HTTP hop around the pure adapters in `ansible_mesh_core::decisions`
//! (all validation, ordering and response checking lives there), plus the
//! vault-key loader for the hotel's OpenRouter key. It lives in its own small
//! crate so both `model-router` (the `model.decisions` controller) and
//! `heal-dispatcher` (the in-process shadow pilot) can use it without either
//! depending on the other. Every failure is a typed `DecisionsError` meaning
//! "use the deterministic decision", and `evaluate` never panics on bad input.

use ansible_mesh_core::decisions::{
    DecisionsError, DecisionsErrorClass, DecisionsOutcome, DecisionsRequest, DecisionsTransport,
    PINNED_OPENROUTER_MODEL, build_wire_request, classify_http_status, parse_wire_response,
};
use std::time::{Duration, Instant};

mod config;
pub mod gate;

pub use config::{DecisionsConfig, load_decisions_config};

const NATIVE_BASE_URL: &str = "https://api.typesafe.ai";
const NATIVE_DEFAULT_MODEL: &str = "jev-latest";
/// Decisions are small and off the model ladder: single-digit seconds per attempt.
pub const DEFAULT_ATTEMPT_SECS: u64 = 8;

/// An outcome plus the bytes that actually left the machine.
#[derive(Debug)]
pub struct Audited {
    pub result: Result<DecisionsOutcome, DecisionsError>,
    /// Length of the request body sent; zero if nothing was sent.
    pub bytes_sent: u64,
}

pub struct DecisionsClient {
    http: reqwest::Client,
    transport: DecisionsTransport,
    base_url: String,
    api_key: Option<String>,
    default_model: String,
}

impl DecisionsClient {
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
        self.evaluate_audited(request, model, timeout).await.result
    }

    /// Like [`evaluate`](Self::evaluate), but also reports how many bytes were
    /// actually put on the wire: the length of the request body that was sent, not
    /// a re-derivation. It is zero whenever nothing left the machine (a policy
    /// refusal, a missing key, a request that failed validation).
    pub async fn evaluate_audited(
        &self,
        request: &DecisionsRequest,
        model: Option<&str>,
        timeout: Duration,
    ) -> Audited {
        let mut bytes_sent = 0;
        let result = self.send(request, model, timeout, &mut bytes_sent).await;
        Audited { result, bytes_sent }
    }

    async fn send(
        &self,
        request: &DecisionsRequest,
        model: Option<&str>,
        timeout: Duration,
        bytes_sent: &mut u64,
    ) -> Result<DecisionsOutcome, DecisionsError> {
        // The data policy holds at this single egress point, for every caller:
        // an unknown or disallowed site, or state that looks like a conversation
        // payload, is refused before any network hop, and every string in `state`
        // is redacted and truncated before it leaves.
        gate::site_spec(&request.site)?;
        gate::screen(request)?;
        let request = &gate::redacted(request);
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

        // From here the body leaves the machine.
        *bytes_sent = body.len() as u64;
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
            site: "smoke.live".into(),
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

    fn provider(base: &str) -> DecisionsClient {
        // The hotel's configured base ends in `/api`; the provider must not double it.
        DecisionsClient::openrouter(
            reqwest::Client::default(),
            Some("test-key".into()),
            Some(format!("{base}/api")),
            None,
        )
    }

    #[test]
    fn openrouter_base_url_never_doubles_the_api_segment() {
        let http = reqwest::Client::default();
        for base in [
            "https://openrouter.ai/api",
            "https://openrouter.ai/api/",
            "https://openrouter.ai",
            "https://openrouter.ai/",
        ] {
            let p = DecisionsClient::openrouter(http.clone(), None, Some(base.into()), None);
            assert_eq!(
                p.url(),
                "https://openrouter.ai/api/alpha/decisions",
                "{base}"
            );
        }
        let default = DecisionsClient::openrouter(http.clone(), None, None, None);
        assert_eq!(default.url(), "https://openrouter.ai/api/alpha/decisions");
        let native = DecisionsClient::native(http, None, None);
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
            DecisionsClient::openrouter(reqwest::Client::default(), Some("  ".into()), None, None);
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
        let p = DecisionsClient::openrouter(
            reqwest::Client::default(),
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
        let p = DecisionsClient::openrouter(
            reqwest::Client::default(),
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
    async fn an_unlisted_site_is_refused_before_any_network_hop() {
        let mut unlisted = request();
        unlisted.site = "memory.recall".into();
        // Nothing listens here; a connection attempt would be `Unavailable`, and a
        // missing key would be `Auth`. Policy must win over both.
        let client = DecisionsClient::openrouter(
            reqwest::Client::default(),
            None,
            Some("http://127.0.0.1:9".into()),
            None,
        );
        let err = client
            .evaluate(&unlisted, None, Duration::from_secs(1))
            .await
            .expect_err("unlisted site");
        assert_eq!(err.class, DecisionsErrorClass::PolicyRefused);
        assert!(err.message.contains("allow-list"), "{}", err.message);
    }

    #[tokio::test]
    async fn secrets_in_state_never_reach_the_wire() {
        let (base, seen) = serve_once("200 OK", OK_BODY).await;
        let mut leaky = request();
        leaky.site = "heal.classify".into();
        leaky.state = json!({
            "guest": "beacon",
            "error": "error sending request for url (https://api.telegram.org/bot123456789:AAE_abcdefghijklmnopqrstuvwxyz012345/getUpdates) Bearer abcdef1234567890xyz",
        });
        provider(&base)
            .evaluate(&leaky, None, Duration::from_secs(5))
            .await
            .expect("call succeeds");

        let raw = seen.await.unwrap();
        for leaked in ["AAE_abcdefghij", "123456789:", "abcdef1234567890xyz"] {
            assert!(!raw.contains(leaked), "`{leaked}` reached the wire: {raw}");
        }
        // What classification needs still arrives.
        assert!(raw.contains("<url:api.telegram.org>"), "{raw}");
        assert!(raw.contains("beacon"));
    }

    #[tokio::test]
    async fn state_that_looks_like_a_conversation_payload_is_refused_and_nothing_is_sent() {
        // A provider error that echoes the failed request, quoted inside a log
        // line, so the JSON arrives escaped.
        let mut echo = request();
        echo.site = "heal.classify".into();
        echo.state = json!({
            "guest": "beacon",
            "error": r#"[beacon][text.generate] openai: 400 {\"messages\":[{\"role\":\"user\",\"content\":\"my private note\"}]}"#,
        });
        let client = DecisionsClient::openrouter(
            reqwest::Client::default(),
            Some("k".into()),
            Some("http://127.0.0.1:9".into()),
            None,
        );
        let audited = client
            .evaluate_audited(&echo, None, Duration::from_secs(1))
            .await;
        let err = audited.result.expect_err("refused");
        assert_eq!(err.class, DecisionsErrorClass::PolicyRefused, "{err}");
        assert_eq!(audited.bytes_sent, 0, "nothing left the machine");
        assert!(!err.message.contains("my private note"), "{}", err.message);
    }

    #[tokio::test]
    async fn bytes_sent_is_the_length_of_the_body_that_actually_went_out() {
        let (base, seen) = serve_once("200 OK", OK_BODY).await;
        let audited = provider(&base)
            .evaluate_audited(&request(), None, Duration::from_secs(5))
            .await;
        assert!(audited.result.is_ok());

        let raw = seen.await.unwrap();
        let (_, body) = raw.split_once("\r\n\r\n").expect("http request");
        assert_eq!(audited.bytes_sent, body.len() as u64);
        assert!(audited.bytes_sent > 0);
    }

    #[tokio::test]
    async fn bytes_are_zero_when_the_request_never_leaves() {
        let client = DecisionsClient::openrouter(reqwest::Client::default(), None, None, None);
        // No key: fails as Auth before anything is put on the wire.
        let audited = client
            .evaluate_audited(&request(), None, Duration::from_secs(1))
            .await;
        assert_eq!(audited.result.unwrap_err().class, DecisionsErrorClass::Auth);
        assert_eq!(audited.bytes_sent, 0);
    }

    /// Live smoke through the real client code. Sends ONE synthetic sentence
    /// (no operator data) with a noul, a choice and a score question. Run with:
    ///
    /// `PHILOTIC_DECISIONS_LIVE_KEY=<dedicated key> cargo test -p decisions-client --lib live_smoke -- --ignored --nocapture`
    ///
    /// Use a dedicated key, never the hotel's vault key. The key is read from the
    /// environment only and is never logged.
    #[tokio::test]
    #[ignore = "needs a real OpenRouter key in PHILOTIC_DECISIONS_LIVE_KEY"]
    async fn live_smoke_openrouter_noul_choice_and_score() {
        let key = std::env::var("PHILOTIC_DECISIONS_LIVE_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .expect("set PHILOTIC_DECISIONS_LIVE_KEY to run the live smoke");
        let request = DecisionsRequest {
            site: "smoke.live".into(),
            state: json!(
                "The payment service has returned connection refused for the last 40 minutes and three retries have failed."
            ),
            questions: vec![
                DecisionQuestion {
                    id: "needs_restart".into(),
                    instructions: "Would restarting the service plausibly fix this?".into(),
                    spec: QuestionSpec::Noul {
                        when_true: Some("A restart would clear the condition".into()),
                        when_false: Some("A restart would not help".into()),
                    },
                },
                DecisionQuestion {
                    id: "severity".into(),
                    instructions: "How severe is this failure?".into(),
                    spec: QuestionSpec::Choice {
                        options: vec![
                            DecisionOption::new("critical", "The service is down"),
                            DecisionOption::new("high", "Degraded and needs attention soon"),
                            DecisionOption::new("low", "Minor and can wait"),
                        ],
                    },
                },
                DecisionQuestion {
                    id: "harm".into(),
                    instructions: "How much user-visible harm has occurred?".into(),
                    spec: QuestionSpec::Score {
                        levels: vec![
                            DecisionOption::new("none", "No visible harm"),
                            DecisionOption::new("minor", "Minor degradation"),
                            DecisionOption::new("major", "Major outage"),
                        ],
                    },
                },
            ],
        };
        let provider =
            DecisionsClient::openrouter(reqwest::Client::default(), Some(key), None, None);
        let outcome = provider
            .evaluate(&request, None, Duration::from_secs(15))
            .await
            .expect("live call succeeds");

        println!(
            "model={} provider={} latency_ms={} usage={:?} request_id={:?}",
            outcome.trace.model,
            outcome.trace.provider,
            outcome.trace.latency_ms,
            outcome.trace.usage,
            outcome.trace.request_id
        );
        assert!(
            outcome.trace.model.starts_with("typesafe/jev-1.13"),
            "the pinned model answered, not the alias: {}",
            outcome.trace.model
        );
        assert!(
            !outcome.trace.legend_mismatch,
            "the score legend echoed our levels"
        );
        assert_eq!(outcome.result.answers.len(), 3);
        assert!(matches!(
            outcome.result.answers["needs_restart"],
            DecisionAnswer::Noul { .. }
        ));
        assert!(matches!(
            outcome.result.answers["severity"],
            DecisionAnswer::Choice { .. }
        ));
        assert!(matches!(
            outcome.result.answers["harm"],
            DecisionAnswer::Score { .. }
        ));
    }
}
