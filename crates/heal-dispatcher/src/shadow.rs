//! Shadow pilot for typed decisions (slice D2): a log-only judge beside the
//! incumbent `gemma3:4b` classifier for novel failure lines.
//!
//! The judge NEVER decides anything. It runs after the incumbent, off the poll
//! loop's path (a spawned task, a small in-flight cap, a short deadline), and
//! writes one content-free `decision_traces` row per call: the incumbent's
//! verdict, the judge's typed answers, whether they agreed, and, when the call
//! failed, the error class. Errors and disagreement are separate columns.
//!
//! Default off. `PHILOTIC_SHADOW_DECISIONS` must be set, the dedicated
//! `decisions` key must be readable by this role, and the site must be on the
//! client's allow-list (`heal.classify`, data class A). The client redacts every
//! string in the state before it leaves (`decisions_client::gate`).

use ansible_mesh_core::decision_trace::{
    DecisionTraceRecord, DecisionTraceStorage, SqliteDecisionTraceStorage, default_db_path,
};
use ansible_mesh_core::decisions::{
    DecisionAnswer, DecisionOption, DecisionQuestion, DecisionsError, DecisionsOutcome,
    DecisionsRequest, QuestionSpec,
};
use decisions_client::{DecisionsClient, gate, load_decisions_config};
use philotic_client::PhiloticClient;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};
use ulid::Ulid;

/// The allow-listed site id (see `decisions_client::gate::SITES`).
pub const SITE: &str = "heal.classify";
const ENV_FLAG: &str = "PHILOTIC_SHADOW_DECISIONS";
/// Shadow calls in flight at once. A burst beyond this is skipped, not queued:
/// the pilot must never build a backlog or slow the poll loop.
const MAX_IN_FLIGHT: usize = 4;
/// One provider attempt. Shadow calls do not retry.
const DEADLINE: Duration = Duration::from_secs(4);

/// What the incumbent classifier decided for the same line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incumbent {
    pub severity: String,
    pub pattern_tag: String,
    pub heal_action: String,
}

/// `PHILOTIC_SHADOW_DECISIONS` is off unless set to a truthy value.
pub fn shadow_enabled(env: impl Fn(&str) -> Option<String>) -> bool {
    env(ENV_FLAG).is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        )
    })
}

pub struct ShadowJudge {
    inner: Arc<Inner>,
}

struct Inner {
    client: DecisionsClient,
    store: Arc<dyn DecisionTraceStorage>,
    permits: Arc<Semaphore>,
}

impl ShadowJudge {
    /// `None` (and no network or IPC cost) unless the flag is set. If it is set
    /// but the key or the trace store is unavailable, warn once and stay off.
    pub async fn init(ipc: &mut PhiloticClient, http: &reqwest::Client) -> Option<Self> {
        if !shadow_enabled(|k| std::env::var(k).ok()) {
            return None;
        }
        let config = match load_decisions_config(ipc).await {
            Ok(config) if config.api_key.is_some() => config,
            Ok(_) => {
                warn!(
                    "{ENV_FLAG} is set but no decisions key is configured \
                     (`phil keys configure decisions`); shadow judge stays off"
                );
                return None;
            }
            Err(e) => {
                warn!(
                    "{ENV_FLAG} is set but the decisions key could not be loaded: {e:#}; shadow judge stays off"
                );
                return None;
            }
        };
        let store = match SqliteDecisionTraceStorage::open(default_db_path()) {
            Ok(store) => Arc::new(store) as Arc<dyn DecisionTraceStorage>,
            Err(e) => {
                warn!("decision trace store unavailable ({e:#}); shadow judge stays off");
                return None;
            }
        };
        let client = DecisionsClient::openrouter(
            http.clone(),
            config.api_key,
            config.base_url,
            config.model,
        );
        info!(
            site = SITE,
            "heal-dispatcher: shadow decisions enabled (log-only)"
        );
        Some(Self::from_parts(client, store))
    }

    pub fn from_parts(client: DecisionsClient, store: Arc<dyn DecisionTraceStorage>) -> Self {
        Self {
            inner: Arc::new(Inner {
                client,
                store,
                permits: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
            }),
        }
    }

    /// Fire and forget. Returns immediately; never fails the caller. If too many
    /// calls are already in flight this one is skipped.
    pub fn observe(&self, guest_id: &str, raw_text: &str, incumbent: Option<Incumbent>) {
        let Ok(permit) = self.inner.permits.clone().try_acquire_owned() else {
            debug!("shadow judge busy; skipping one observation");
            return;
        };
        let inner = self.inner.clone();
        let guest_id = guest_id.to_string();
        let raw_text = raw_text.to_string();
        tokio::spawn(async move {
            let _permit = permit;
            inner.run(&guest_id, &raw_text, incumbent).await;
        });
    }
}

impl Inner {
    async fn run(&self, guest_id: &str, raw_text: &str, incumbent: Option<Incumbent>) {
        let request = build_request(guest_id, raw_text);
        // What the client will actually send, for the audit row's byte count.
        let bytes_sent = gate::egress_bytes(&gate::redacted(&request)) as u64;
        let result = self.client.evaluate(&request, None, DEADLINE).await;
        let record = trace_record(guest_id, bytes_sent, incumbent.as_ref(), &result);
        if let Err(e) = self.store.record_trace(&record) {
            warn!("decision trace write failed: {e:#}");
        }
    }
}

/// The typed questions for a failure line. The state is the guest id and the raw
/// text; the client redacts and truncates it.
pub fn build_request(guest_id: &str, raw_text: &str) -> DecisionsRequest {
    DecisionsRequest {
        site: SITE.into(),
        state: json!({ "guest": guest_id, "error": raw_text }),
        questions: vec![
            DecisionQuestion {
                id: "severity".into(),
                instructions: "How severe is this guest process error for the operator's system?"
                    .into(),
                spec: QuestionSpec::Choice {
                    options: vec![
                        DecisionOption::new("critical", "The service is down or data is at risk"),
                        DecisionOption::new("high", "Degraded and needs attention soon"),
                        DecisionOption::new("medium", "Noticeable but not urgent"),
                        DecisionOption::new("low", "Minor or cosmetic"),
                    ],
                },
            },
            DecisionQuestion {
                id: "needs_restart".into(),
                instructions: "Would restarting the guest process plausibly resolve this error?"
                    .into(),
                spec: QuestionSpec::Noul {
                    when_true: Some(
                        "A transient or stuck-process condition that a restart clears".into(),
                    ),
                    when_false: Some(
                        "A restart would not help: configuration, credentials, an upstream service, or benign"
                            .into(),
                    ),
                },
            },
        ],
    }
}

/// Did the judge agree with the incumbent, per question? `None` = not
/// comparable. The 0.5 on `needs_restart` is a logging convenience so a row can
/// say "agreed"; it is NOT a decision threshold, and the raw probability is
/// stored beside it for calibration.
pub fn compare(
    incumbent: &Incumbent,
    outcome: &DecisionsOutcome,
) -> BTreeMap<String, Option<bool>> {
    let mut agreement = BTreeMap::new();

    let severity = match outcome.result.answers.get("severity") {
        Some(DecisionAnswer::Choice { choice, .. })
            if matches!(
                incumbent.severity.as_str(),
                "critical" | "high" | "medium" | "low"
            ) =>
        {
            Some(*choice == incumbent.severity)
        }
        _ => None,
    };
    agreement.insert("severity".to_string(), severity);

    let restart = match outcome.result.answers.get("needs_restart") {
        Some(DecisionAnswer::Noul { noul })
            if matches!(
                incumbent.heal_action.as_str(),
                "restart_guest" | "escalate" | "noop"
            ) =>
        {
            Some((*noul > 0.5) == (incumbent.heal_action == "restart_guest"))
        }
        _ => None,
    };
    agreement.insert("needs_restart".to_string(), restart);
    agreement
}

fn trace_record(
    guest_id: &str,
    bytes_sent: u64,
    incumbent: Option<&Incumbent>,
    result: &Result<DecisionsOutcome, DecisionsError>,
) -> DecisionTraceRecord {
    let incumbent_json = incumbent.map(|i| {
        json!({
            "severity": i.severity,
            "pattern_tag": i.pattern_tag,
            "heal_action": i.heal_action,
        })
    });
    let base = DecisionTraceRecord {
        trace_id: Ulid::new().to_string(),
        timestamp: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        site: SITE.into(),
        data_class: gate::site_spec(SITE)
            .map_or("A", |s| s.class.as_str())
            .into(),
        bytes_sent,
        outcome: "ok".into(),
        error_class: None,
        provider: None,
        model: None,
        transport: None,
        latency_ms: None,
        input_tokens: None,
        output_tokens: None,
        cost_usd: None,
        legend_mismatch: false,
        request_id: None,
        subject: Some(guest_id.to_string()),
        incumbent: incumbent_json,
        answers: None,
        agreement: BTreeMap::new(),
    };
    match result {
        Ok(outcome) => DecisionTraceRecord {
            provider: Some(outcome.trace.provider.clone()),
            model: Some(outcome.trace.model.clone()),
            transport: Some(outcome.trace.transport.as_str().to_string()),
            latency_ms: Some(outcome.trace.latency_ms),
            input_tokens: Some(outcome.trace.usage.input_tokens),
            output_tokens: Some(outcome.trace.usage.output_tokens),
            cost_usd: outcome.trace.usage.cost_usd,
            legend_mismatch: outcome.trace.legend_mismatch,
            request_id: outcome.trace.request_id.clone(),
            answers: serde_json::to_value(&outcome.result.answers).ok(),
            agreement: incumbent.map(|i| compare(i, outcome)).unwrap_or_default(),
            ..base
        },
        Err(error) => DecisionTraceRecord {
            outcome: "error".into(),
            error_class: Some(error.class.as_str().to_string()),
            ..base
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::decisions::{
        DecisionsErrorClass, DecisionsResult, DecisionsTrace, DecisionsTransport,
        DecisionsTransport as T, DecisionsUsage, build_wire_request,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn incumbent(severity: &str, action: &str) -> Incumbent {
        Incumbent {
            severity: severity.into(),
            pattern_tag: "connection_refused".into(),
            heal_action: action.into(),
        }
    }

    fn outcome(choice: &str, needs_restart: f64) -> DecisionsOutcome {
        DecisionsOutcome {
            result: DecisionsResult {
                site: SITE.into(),
                answers: BTreeMap::from([
                    (
                        "severity".to_string(),
                        DecisionAnswer::Choice {
                            choice: choice.into(),
                            probabilities: BTreeMap::from([(choice.to_string(), 0.9)]),
                            confidence: 0.9,
                        },
                    ),
                    (
                        "needs_restart".to_string(),
                        DecisionAnswer::Noul {
                            noul: needs_restart,
                        },
                    ),
                ]),
            },
            trace: DecisionsTrace {
                provider: "TypeSafe".into(),
                model: "typesafe/jev-1.13-20260917".into(),
                transport: DecisionsTransport::OpenRouter,
                latency_ms: 300,
                usage: DecisionsUsage {
                    input_tokens: 400,
                    output_tokens: 60,
                    cost_usd: Some(0.0000168),
                },
                request_id: Some("gen-dec-x".into()),
                legend_mismatch: false,
            },
        }
    }

    #[test]
    fn the_flag_is_off_unless_explicitly_truthy() {
        let with = |v: Option<&'static str>| shadow_enabled(move |_| v.map(str::to_string));
        assert!(!with(None));
        assert!(!with(Some("")));
        assert!(!with(Some("0")));
        assert!(!with(Some("false")));
        assert!(!with(Some("maybe")));
        for on in ["1", "true", "TRUE", " on ", "yes"] {
            assert!(with(Some(on)), "{on}");
        }
    }

    #[test]
    fn the_request_is_on_the_allow_list_valid_and_ordered() {
        let request = build_request("beacon", "connection refused");
        gate::site_spec(&request.site).expect("heal.classify is allow-listed");
        build_wire_request(T::OpenRouter, &request, "typesafe/jev-1.13")
            .expect("valid on the wire");
        let ids: Vec<&str> = request.questions.iter().map(|q| q.id.as_str()).collect();
        assert_eq!(ids, ["severity", "needs_restart"]);
        assert_eq!(request.state["guest"], "beacon");
    }

    #[test]
    fn agreement_is_per_question_and_none_when_not_comparable() {
        // Judge says high, restart unlikely.
        let judged = outcome("high", 0.1);
        let a = compare(&incumbent("high", "escalate"), &judged);
        assert_eq!(a["severity"], Some(true));
        assert_eq!(
            a["needs_restart"],
            Some(true),
            "escalate is a non-restart, judge agrees"
        );

        let a = compare(&incumbent("medium", "restart_guest"), &judged);
        assert_eq!(a["severity"], Some(false));
        assert_eq!(
            a["needs_restart"],
            Some(false),
            "incumbent restarts, judge does not"
        );

        // "unknown" is outside the judge's options: not comparable, not disagreement.
        let a = compare(&incumbent("unknown", "noop"), &judged);
        assert_eq!(a["severity"], None);
        assert_eq!(a["needs_restart"], Some(true));

        let a = compare(&incumbent("high", "something_new"), &judged);
        assert_eq!(a["needs_restart"], None);
    }

    #[test]
    fn a_trace_row_holds_provenance_and_answers_but_never_the_text_sent() {
        let secret_line = "boom sk-or-v1-0123456789abcdef at /Users/jaredlikes/x";
        let request = build_request("beacon", secret_line);
        let bytes = gate::egress_bytes(&gate::redacted(&request)) as u64;
        let record = trace_record(
            "beacon",
            bytes,
            Some(&incumbent("high", "restart_guest")),
            &Ok(outcome("high", 0.8)),
        );
        assert_eq!(record.site, "heal.classify");
        assert_eq!(record.data_class, "A");
        assert_eq!(record.outcome, "ok");
        assert_eq!(record.model.as_deref(), Some("typesafe/jev-1.13-20260917"));
        assert_eq!(record.agreement["severity"], Some(true));
        assert!(record.bytes_sent > 0);
        let stored = serde_json::to_string(&record).unwrap();
        assert!(
            !stored.contains("sk-or-v1")
                && !stored.contains("jaredlikes")
                && !stored.contains("boom"),
            "{stored}"
        );
    }

    #[test]
    fn a_failed_call_is_an_error_row_with_no_agreement() {
        let failed = Err(DecisionsError::new(
            DecisionsErrorClass::RateLimited,
            "slow",
        ));
        let record = trace_record("beacon", 100, Some(&incumbent("high", "noop")), &failed);
        assert_eq!(record.outcome, "error");
        assert_eq!(record.error_class.as_deref(), Some("rate_limited"));
        assert!(record.agreement.is_empty() && record.answers.is_none() && record.model.is_none());
        assert_eq!(record.incumbent.as_ref().unwrap()["heal_action"], "noop");
    }

    // ── end to end against a local socket ────────────────────────────────────

    const OK_BODY: &str = r#"{"model":"typesafe/jev-1.13-20260917","answers":{"severity":{"type":"choice","choice":"high","probabilities":{"critical":0.05,"high":0.9,"medium":0.03,"low":0.02},"confidence":0.9},"needs_restart":{"type":"noul","noul":0.8}},"usage":{"input_tokens":400,"output_tokens":60,"cost":0.0000168},"id":"gen-dec-x","provider":"TypeSafe"}"#;

    /// Serve one canned response, or (when `hang`) accept and never answer.
    async fn serve(status_line: &'static str, body: &'static str, hang: bool) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let _ = socket.read(&mut buf).await;
            if hang {
                tokio::time::sleep(Duration::from_secs(60)).await;
                return;
            }
            let response = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        });
        format!("http://{addr}")
    }

    fn judge(base: &str, dir: &std::path::Path) -> (ShadowJudge, Arc<SqliteDecisionTraceStorage>) {
        let store = Arc::new(SqliteDecisionTraceStorage::open(dir.join("d.db")).unwrap());
        let client = DecisionsClient::openrouter(
            reqwest::Client::new(),
            Some("test-key".into()),
            Some(format!("{base}/api")),
            None,
        );
        (ShadowJudge::from_parts(client, store.clone()), store)
    }

    async fn wait_for_rows(
        store: &SqliteDecisionTraceStorage,
        n: usize,
    ) -> Vec<DecisionTraceRecord> {
        for _ in 0..100 {
            let rows = store.list_traces(10).unwrap();
            if rows.len() >= n {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("no trace row appeared");
    }

    #[tokio::test]
    async fn observe_records_an_ok_row_with_agreement() {
        let base = serve("200 OK", OK_BODY, false).await;
        let dir = tempfile::tempdir().unwrap();
        let (judge, store) = judge(&base, dir.path());

        judge.observe(
            "beacon",
            "connection refused",
            Some(incumbent("high", "restart_guest")),
        );
        let rows = wait_for_rows(&store, 1).await;
        let row = &rows[0];
        assert_eq!(row.outcome, "ok");
        assert_eq!(row.agreement["severity"], Some(true));
        assert_eq!(row.agreement["needs_restart"], Some(true));
        assert_eq!(row.model.as_deref(), Some("typesafe/jev-1.13-20260917"));
        assert_eq!(row.transport.as_deref(), Some("openrouter"));
        assert!(row.answers.is_some());
    }

    #[tokio::test]
    async fn a_provider_error_is_an_error_row_not_a_disagreement() {
        let base = serve("429 Too Many Requests", "slow down", false).await;
        let dir = tempfile::tempdir().unwrap();
        let (judge, store) = judge(&base, dir.path());

        judge.observe("beacon", "novel failure", Some(incumbent("high", "noop")));
        let rows = wait_for_rows(&store, 1).await;
        assert_eq!(rows[0].outcome, "error");
        assert_eq!(rows[0].error_class.as_deref(), Some("rate_limited"));
        assert!(rows[0].agreement.is_empty());
    }

    #[tokio::test]
    async fn observe_never_blocks_even_when_the_provider_hangs() {
        let base = serve("200 OK", OK_BODY, true).await;
        let dir = tempfile::tempdir().unwrap();
        let (judge, _store) = judge(&base, dir.path());

        let started = std::time::Instant::now();
        judge.observe("beacon", "novel failure", None);
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "observe must return immediately, took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn observations_beyond_the_in_flight_cap_are_skipped_not_queued() {
        let base = serve("200 OK", OK_BODY, true).await;
        let dir = tempfile::tempdir().unwrap();
        let (judge, _store) = judge(&base, dir.path());
        // Hold every permit, as if MAX_IN_FLIGHT slow calls were outstanding.
        let held: Vec<_> = (0..MAX_IN_FLIGHT)
            .map(|_| judge.inner.permits.clone().try_acquire_owned().unwrap())
            .collect();
        judge.observe("beacon", "one more", None);
        assert_eq!(judge.inner.permits.available_permits(), 0);
        drop(held);
        assert_eq!(judge.inner.permits.available_permits(), MAX_IN_FLIGHT);
    }
}
