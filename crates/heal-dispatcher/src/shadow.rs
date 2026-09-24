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
//! OpenRouter key must be readable by this role (`aiua auth sync-roles`), and the site must be on the
//! client's allow-list (`heal.classify`, data class A). The client redacts every
//! string in the state before it leaves (`decisions_client::gate`).

use ansible_mesh_core::decision_trace::{DecisionSummary, summarize};
use ansible_mesh_core::decision_trace::{
    DecisionTraceRecord, DecisionTraceStorage, SqliteDecisionTraceStorage, default_db_path,
};
use ansible_mesh_core::decisions::{
    DecisionAnswer, DecisionOption, DecisionQuestion, DecisionsError, DecisionsErrorClass,
    DecisionsOutcome, DecisionsRequest, QuestionSpec,
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
/// Only this many trailing characters of a failure line are ever asked about.
const MAX_LINE_CHARS: usize = 500;

/// Model capabilities whose failure lines are never sent. `model-router`'s
/// `emit_failure` is the only source of untriaged model failures, and its text is
/// `[guest][capability] provider: <provider error body>`; a provider's error body
/// can echo part of the request that failed, i.e. conversation content. Those
/// lines are excluded outright rather than trusted to redaction.
const MODEL_CAPABILITIES: &[&str] = &[
    "text.generate",
    "response.generate",
    "voice.dialogue",
    "voice.transcribe",
    "voice.synthesize",
    "media.analyze",
    "text.embed",
    "decisions.evaluate",
];

/// `[guest][capability] rest` gives `capability`, when the line has that envelope.
fn envelope_capability(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('[')?;
    let (_guest, rest) = rest.split_once("][")?;
    let (capability, _) = rest.split_once(']')?;
    Some(capability)
}

/// Is this a model-controller failure line (a provider error body may be in it)?
fn is_model_failure_line(line: &str) -> bool {
    envelope_capability(line).is_some_and(|c| MODEL_CAPABILITIES.contains(&c))
}

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

/// The typed-decisions judge for failure lines. Two independent modes:
///
/// - **shadow** (`PHILOTIC_SHADOW_DECISIONS`): log-only, beside the incumbent;
///   see [`DecisionsJudge::observe`].
/// - **fallback** (`PHILOTIC_HEAL_DECISIONS_FALLBACK`): when the incumbent has no
///   verdict (Ollama down or its breaker open), the judge's answer IS applied —
///   bounded to tagging + severity, never a restart; see
///   [`DecisionsJudge::classify_fallback`].
const FALLBACK_ENV_FLAG: &str = "PHILOTIC_HEAL_DECISIONS_FALLBACK";
/// Fallback calls allowed per wall-clock minute; beyond it rows keep the old
/// `unclassified`/noop fail-safe, so an Ollama outage cannot stall the loop.
const FALLBACK_CALLS_PER_MINUTE: u32 = 30;
/// A tag is applied only when its probability reaches this; below it the row
/// stays `unclassified`. Call-site threshold (invariant: thresholds live in
/// code, calibrated per question from `decision_traces`).
pub const FALLBACK_MIN_TAG_PROBABILITY: f64 = 0.6;
/// Option key meaning "none of the listed patterns".
pub const OTHER_TAG: &str = "other";

/// The patterns the fallback may assign: `(tag, description)`. Every tag maps
/// through `heal_action_for_pattern_tag`; none maps to `restart_guest`, and the
/// verdict caps any restart to `escalate` regardless.
pub const FALLBACK_TAGS: &[(&str, &str)] = &[
    (
        "telegram_poll_conflict",
        "Telegram 409 Conflict: another process is polling the same bot token (getUpdates)",
    ),
    (
        "external_api_4xx",
        "A non-model external API (Telegram, Discord, an integration) rejected our request: 4xx",
    ),
    (
        "external_api_5xx",
        "A non-model external API or upstream service failed or was unreachable: 5xx, timeout, connection refused",
    ),
    (
        "service_probe_failed",
        "A local dependency service (memory store, database, sidecar) is unreachable",
    ),
    (
        "delivery_channel_closed",
        "A message or task could not be delivered because a channel or socket was closed",
    ),
    (
        "config_error",
        "Missing or invalid configuration, credential, token, or permission",
    ),
    (
        "benign_log",
        "Informational or expected noise: a retry notice, back-off, shutdown, or a warning with no failure",
    ),
    (OTHER_TAG, "None of the above"),
];

/// `PHILOTIC_HEAL_DECISIONS_FALLBACK` is off unless set to a truthy value.
pub fn fallback_enabled(env: impl Fn(&str) -> Option<String>) -> bool {
    env(FALLBACK_ENV_FLAG).is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        )
    })
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub struct DecisionsJudge {
    inner: Arc<Inner>,
    shadow_on: bool,
    fallback_on: bool,
    /// `(minute, calls)` — fallback calls are budgeted per wall-clock minute.
    fallback_budget: std::sync::Mutex<(u64, u32)>,
}

struct Inner {
    client: DecisionsClient,
    store: Arc<dyn DecisionTraceStorage>,
    permits: Arc<Semaphore>,
}

impl DecisionsJudge {
    /// `None` (and no network or IPC cost) unless the flag is set. If it is set
    /// but the key or the trace store is unavailable, warn once and stay off.
    pub async fn init(ipc: &mut PhiloticClient, http: &reqwest::Client) -> Option<Self> {
        let shadow_on = shadow_enabled(|k| std::env::var(k).ok());
        let fallback_on = fallback_enabled(|k| std::env::var(k).ok());
        if !shadow_on && !fallback_on {
            return None;
        }
        let config = match load_decisions_config(ipc).await {
            Ok(config) if config.api_key.is_some() => config,
            Ok(_) => {
                warn!(
                    "{ENV_FLAG} is set but no OpenRouter key is configured \
                     (`phil keys configure openrouter`); shadow judge stays off"
                );
                return None;
            }
            Err(e) => {
                warn!(
                    "{ENV_FLAG} is set but the OpenRouter key could not be loaded: {e:#}. If it is \
                     an access denial, run `aiua auth sync-roles --provider openrouter --db <context db>` \
                     on this hotel; shadow judge stays off"
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
            shadow = shadow_on,
            fallback = fallback_on,
            "heal-dispatcher: decisions judge enabled"
        );
        Some(Self::from_parts(client, store).with_modes(shadow_on, fallback_on))
    }

    /// Shadow mode only (the D2 default); see [`Self::with_modes`].
    pub fn from_parts(client: DecisionsClient, store: Arc<dyn DecisionTraceStorage>) -> Self {
        Self {
            inner: Arc::new(Inner {
                client,
                store,
                permits: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
            }),
            shadow_on: true,
            fallback_on: false,
            fallback_budget: std::sync::Mutex::new((0, 0)),
        }
    }

    pub fn with_modes(mut self, shadow_on: bool, fallback_on: bool) -> Self {
        self.shadow_on = shadow_on;
        self.fallback_on = fallback_on;
        self
    }

    pub fn fallback_on(&self) -> bool {
        self.fallback_on
    }

    /// Classify a failure line the incumbent could not (slice H1). Returns
    /// `None` — leaving the caller on its old fail-safe — when fallback mode is
    /// off, the line is a model-failure line, the minute's budget is spent, or
    /// the call fails. Awaited inline: the dispatcher is off the hot path and
    /// the call is capped at [`DEADLINE`]. Every call writes a trace row keyed
    /// by the heal row id.
    pub async fn classify_fallback(
        &self,
        row_id: &str,
        guest_id: &str,
        raw_text: &str,
    ) -> Option<(String, String, String)> {
        if !self.fallback_on {
            return None;
        }
        let refused = is_model_failure_line(raw_text);
        if !refused && !self.take_fallback_budget(unix_secs()) {
            debug!("decisions fallback budget spent this minute; leaving row unclassified");
            return None;
        }
        let (bytes_sent, result) = if refused {
            (
                0,
                Err(DecisionsError::new(
                    DecisionsErrorClass::PolicyRefused,
                    "model-controller failure line: a provider error body can echo the request",
                )),
            )
        } else {
            let request = build_fallback_request(guest_id, &gate::tail(raw_text, MAX_LINE_CHARS));
            let audited = self
                .inner
                .client
                .evaluate_audited(&request, None, DEADLINE)
                .await;
            (audited.bytes_sent, audited.result)
        };
        let verdict = result
            .as_ref()
            .ok()
            .map(|outcome| fallback_verdict(&outcome.result.answers));
        let mut record = trace_record(guest_id, bytes_sent, None, &result);
        record.incumbent = Some(json!({
            "mode": "fallback",
            "row_id": row_id,
            "applied": verdict.as_ref().map(|(sev, tag, action)| json!({
                "severity": sev, "pattern_tag": tag, "heal_action": action,
            })),
        }));
        if let Err(e) = self.inner.store.record_trace(&record) {
            warn!("decision trace write failed: {e:#}");
        }
        verdict
    }

    fn take_fallback_budget(&self, now_secs: u64) -> bool {
        let minute = now_secs / 60;
        let mut budget = self
            .fallback_budget
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if budget.0 != minute {
            *budget = (minute, 0);
        }
        if budget.1 >= FALLBACK_CALLS_PER_MINUTE {
            return false;
        }
        budget.1 += 1;
        true
    }

    /// Fire and forget. Returns immediately; never fails the caller. If too many
    /// calls are already in flight this one is skipped.
    pub fn observe(&self, guest_id: &str, raw_text: &str, incumbent: Option<Incumbent>) {
        if !self.shadow_on {
            return;
        }
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
        // Model-controller failure lines carry provider error bodies that can echo
        // the failed request. They are never sent; the skip is recorded so the
        // operator can see how often it happens.
        if is_model_failure_line(raw_text) {
            let refused = Err(DecisionsError::new(
                DecisionsErrorClass::PolicyRefused,
                "model-controller failure line: a provider error body can echo the request",
            ));
            let record = trace_record(guest_id, 0, incumbent.as_ref(), &refused);
            if let Err(e) = self.store.record_trace(&record) {
                warn!("decision trace write failed: {e:#}");
            }
            return;
        }
        // Only the tail of the line is asked about. Heal classification needs the
        // error, not the whole message, and every extra character is exposure.
        let request = build_request(guest_id, &gate::tail(raw_text, MAX_LINE_CHARS));
        // The byte count is what the client actually put on the wire (zero when the
        // data policy refused), not a second derivation.
        let audited = self.client.evaluate_audited(&request, None, DEADLINE).await;
        let record = trace_record(
            guest_id,
            audited.bytes_sent,
            incumbent.as_ref(),
            &audited.result,
        );
        if let Err(e) = self.store.record_trace(&record) {
            warn!("decision trace write failed: {e:#}");
        }
    }
}

/// Render a summary of the shadow run: errors, skipped and disagreement are kept
/// on separate lines so an outage or a policy refusal never reads as the judge
/// disagreeing.
pub fn format_summary(s: &DecisionSummary) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "decision traces: {} rows ({} ok, {} legend mismatches), {} bytes sent, ${:.6} spent\n",
        s.total, s.ok, s.legend_mismatches, s.total_bytes_sent, s.total_cost_usd
    ));
    let list = |m: &BTreeMap<String, u64>| {
        if m.is_empty() {
            "none".to_string()
        } else {
            m.iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    out.push_str(&format!("errors by class: {}\n", list(&s.errors_by_class)));
    out.push_str(&format!(
        "skipped by policy: {}\n",
        list(&s.skipped_by_reason)
    ));
    if s.agreement.is_empty() {
        out.push_str("agreement: no comparable rows yet\n");
    }
    for (question, (agreed, compared)) in &s.agreement {
        let pct = 100.0 * *agreed as f64 / (*compared).max(1) as f64;
        out.push_str(&format!(
            "agreement[{question}]: {agreed}/{compared} ({pct:.0}%)\n"
        ));
    }
    out
}

/// `heal-dispatcher --decision-summary`: read `decision_traces.db` and print the
/// summary. Read-only: it does not create the database if it is absent.
pub fn print_summary() -> anyhow::Result<()> {
    let path = default_db_path();
    if !path.exists() {
        println!(
            "no decision traces yet ({} does not exist). Set {ENV_FLAG}=1 on this heal-dispatcher and let it run.",
            path.display()
        );
        return Ok(());
    }
    let store = SqliteDecisionTraceStorage::open(&path)?;
    let records = store.list_traces(100_000)?;
    println!("store: {}", path.display());
    print!("{}", format_summary(&summarize(&records)));
    Ok(())
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

/// The fallback's questions: the shadow's severity choice plus a pattern choice
/// over [`FALLBACK_TAGS`]. Same site and state as [`build_request`].
pub fn build_fallback_request(guest_id: &str, raw_text: &str) -> DecisionsRequest {
    let mut request = build_request(guest_id, raw_text);
    request.questions.retain(|q| q.id == "severity");
    request.questions.push(DecisionQuestion {
        id: "pattern".into(),
        instructions: "Which kind of failure is this guest process error?".into(),
        spec: QuestionSpec::Choice {
            options: FALLBACK_TAGS
                .iter()
                .map(|(key, description)| DecisionOption::new(*key, *description))
                .collect(),
        },
    });
    request
}

/// Map the fallback's answers to `(severity, pattern_tag, heal_action)`.
/// Below [`FALLBACK_MIN_TAG_PROBABILITY`], or on `other`, the tag stays
/// `unclassified`. The action comes from the deterministic tag table and is
/// capped at `escalate`: the judge may label and route a line, never restart.
pub fn fallback_verdict(answers: &BTreeMap<String, DecisionAnswer>) -> (String, String, String) {
    let severity = match answers.get("severity") {
        Some(DecisionAnswer::Choice { choice, .. }) => choice.clone(),
        _ => "unknown".to_string(),
    };
    let tag = match answers.get("pattern") {
        Some(DecisionAnswer::Choice {
            choice,
            probabilities,
            ..
        }) if choice != OTHER_TAG
            && probabilities.get(choice).copied().unwrap_or(0.0)
                >= FALLBACK_MIN_TAG_PROBABILITY =>
        {
            choice.clone()
        }
        _ => "unclassified".to_string(),
    };
    let action = match ansible_mesh_core::heal_queue::heal_action_for_pattern_tag(&tag) {
        "restart_guest" => "escalate",
        other => other,
    };
    (severity, tag, action.to_string())
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
        // The data policy declined to send: nothing left the machine. Recorded as
        // `skipped`, never as an error or a disagreement.
        Err(error) if error.class == DecisionsErrorClass::PolicyRefused => DecisionTraceRecord {
            outcome: "skipped".into(),
            error_class: Some(error.class.as_str().to_string()),
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
    fn a_trace_row_holds_provenance_and_answers() {
        let record = trace_record(
            "beacon",
            512,
            Some(&incumbent("high", "restart_guest")),
            &Ok(outcome("high", 0.8)),
        );
        assert_eq!(record.site, "heal.classify");
        assert_eq!(record.data_class, "A");
        assert_eq!(record.outcome, "ok");
        assert_eq!(record.model.as_deref(), Some("typesafe/jev-1.13-20260917"));
        assert_eq!(record.agreement["severity"], Some(true));
        assert_eq!(record.bytes_sent, 512);
    }

    #[test]
    fn a_policy_refusal_is_a_skipped_row_that_sent_nothing() {
        let refused = Err(DecisionsError::new(
            DecisionsErrorClass::PolicyRefused,
            "state resembles a conversation payload",
        ));
        let record = trace_record("beacon", 0, Some(&incumbent("high", "noop")), &refused);
        assert_eq!(record.outcome, "skipped");
        assert_eq!(record.error_class.as_deref(), Some("policy_refused"));
        assert_eq!(record.bytes_sent, 0);
        assert!(record.agreement.is_empty());
    }

    #[test]
    fn a_line_that_echoes_a_conversation_is_skipped_end_to_end_and_never_sent() {
        // An unreachable base: if the judge tried to send, this would be an
        // `unavailable` error row, not a `skipped` one.
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteDecisionTraceStorage::open(dir.path().join("d.db")).unwrap());
        let client = DecisionsClient::openrouter(
            reqwest::Client::default(),
            Some("k".into()),
            Some("http://127.0.0.1:9".into()),
            None,
        );
        let judge = DecisionsJudge::from_parts(client, store.clone());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            judge
                .inner
                .run(
                    "beacon",
                    r#"[beacon][text.generate] openai: 400 {"messages":[{"role":"user","content":"private"}]}"#,
                    None,
                )
                .await;
        });
        let rows = store.list_traces(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome, "skipped");
        assert_eq!(rows[0].error_class.as_deref(), Some("policy_refused"));
        assert_eq!(rows[0].bytes_sent, 0);
        assert!(!serde_json::to_string(&rows[0]).unwrap().contains("private"));
    }

    #[test]
    fn model_failure_envelopes_are_recognised_and_other_lines_are_not() {
        // The exact shape model-router's emit_failure pushes.
        for capability in MODEL_CAPABILITIES {
            let line = format!("[model-controller-openai-01][{capability}] openai: HTTP 400 x");
            assert_eq!(envelope_capability(&line), Some(*capability), "{line}");
            assert!(is_model_failure_line(&line), "{line}");
        }
        for line in [
            "thread 'main' panicked at src/main.rs:42",
            "connection refused",
            "[beacon] started",
            "[membrane][telegram.poll] getUpdates failed",
            "",
        ] {
            assert!(!is_model_failure_line(line), "{line:?}");
        }
    }

    #[test]
    fn a_model_failure_line_is_skipped_without_any_network_hop() {
        // An unreachable base: an attempted send would be an `unavailable` error row.
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteDecisionTraceStorage::open(dir.path().join("d.db")).unwrap());
        let client = DecisionsClient::openrouter(
            reqwest::Client::default(),
            Some("k".into()),
            Some("http://127.0.0.1:9".into()),
            None,
        );
        let judge = DecisionsJudge::from_parts(client, store.clone());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            judge
                .inner
                .run(
                    "model-controller-openai-01",
                    "[model-controller-openai-01][text.generate] openai: HTTP 400 weird error",
                    Some(incumbent("unknown", "noop")),
                )
                .await;
        });
        let rows = store.list_traces(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome, "skipped");
        assert_eq!(rows[0].error_class.as_deref(), Some("policy_refused"));
        assert_eq!(rows[0].bytes_sent, 0);
    }

    #[test]
    fn only_the_tail_of_a_long_line_is_asked_about() {
        let long = format!("{}THE-ERROR", "x".repeat(MAX_LINE_CHARS * 3));
        let tail = gate::tail(&long, MAX_LINE_CHARS);
        assert!(tail.ends_with("THE-ERROR"));
        assert_eq!(tail.chars().count(), MAX_LINE_CHARS + 1);
    }

    #[test]
    fn the_summary_keeps_errors_skips_and_agreement_on_separate_lines() {
        let summary = summarize(&[
            {
                let mut r = trace_record(
                    "beacon",
                    600,
                    Some(&incumbent("high", "restart_guest")),
                    &Ok(outcome("high", 0.9)),
                );
                r.trace_id = "a".into();
                r
            },
            {
                let mut r = trace_record(
                    "beacon",
                    600,
                    None,
                    &Err(DecisionsError::new(DecisionsErrorClass::Timeout, "slow")),
                );
                r.trace_id = "b".into();
                r
            },
            {
                let mut r = trace_record(
                    "beacon",
                    0,
                    None,
                    &Err(DecisionsError::new(
                        DecisionsErrorClass::PolicyRefused,
                        "no",
                    )),
                );
                r.trace_id = "c".into();
                r
            },
        ]);
        let text = format_summary(&summary);
        assert!(text.contains("3 rows (1 ok"), "{text}");
        assert!(text.contains("errors by class: timeout=1"), "{text}");
        assert!(
            text.contains("skipped by policy: policy_refused=1"),
            "{text}"
        );
        assert!(text.contains("agreement[severity]: 1/1 (100%)"), "{text}");
        assert!(text.contains("1200 bytes sent"), "{text}");
        assert!(format_summary(&DecisionSummary::default()).contains("no comparable rows yet"));
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

    fn judge(
        base: &str,
        dir: &std::path::Path,
    ) -> (DecisionsJudge, Arc<SqliteDecisionTraceStorage>) {
        let store = Arc::new(SqliteDecisionTraceStorage::open(dir.join("d.db")).unwrap());
        let client = DecisionsClient::openrouter(
            reqwest::Client::default(),
            Some("test-key".into()),
            Some(format!("{base}/api")),
            None,
        );
        (DecisionsJudge::from_parts(client, store.clone()), store)
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

    // ── fallback mode (slice H1) ─────────────────────────────────────────────

    fn choice(choice: &str, p: &[(&str, f64)]) -> DecisionAnswer {
        DecisionAnswer::Choice {
            choice: choice.into(),
            probabilities: p.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            confidence: p.iter().map(|(_, v)| *v).fold(0.0, f64::max),
        }
    }

    #[test]
    fn fallback_verdict_applies_a_confident_tag_through_the_action_table() {
        let answers = BTreeMap::from([
            ("severity".to_string(), choice("high", &[("high", 0.8)])),
            (
                "pattern".to_string(),
                choice("telegram_poll_conflict", &[("telegram_poll_conflict", 0.9)]),
            ),
        ]);
        assert_eq!(
            fallback_verdict(&answers),
            (
                "high".into(),
                "telegram_poll_conflict".into(),
                "escalate".into()
            )
        );
    }

    #[test]
    fn fallback_verdict_keeps_unclassified_below_threshold_or_on_other() {
        let unsure = BTreeMap::from([(
            "pattern".to_string(),
            choice("config_error", &[("config_error", 0.4)]),
        )]);
        assert_eq!(fallback_verdict(&unsure).1, "unclassified");
        assert_eq!(fallback_verdict(&unsure).0, "unknown");
        let other = BTreeMap::from([(
            "pattern".to_string(),
            choice(OTHER_TAG, &[(OTHER_TAG, 0.99)]),
        )]);
        assert_eq!(fallback_verdict(&other).1, "unclassified");
        assert_eq!(fallback_verdict(&other).2, "noop");
    }

    #[test]
    fn no_fallback_tag_can_restart_a_guest() {
        for (tag, _) in FALLBACK_TAGS {
            let answers = BTreeMap::from([("pattern".to_string(), choice(tag, &[(tag, 1.0)]))]);
            assert_ne!(fallback_verdict(&answers).2, "restart_guest", "{tag}");
        }
    }

    #[test]
    fn fallback_request_is_valid_and_asks_severity_and_pattern() {
        let request = build_fallback_request("mac-jane:membrane-gateway", "Conflict: 409");
        request.validate(T::OpenRouter).expect("valid envelope");
        let ids: Vec<_> = request.questions.iter().map(|q| q.id.as_str()).collect();
        assert_eq!(ids, ["severity", "pattern"]);
        assert_eq!(request.site, SITE);
    }

    #[test]
    fn fallback_budget_resets_each_minute() {
        let dir = tempfile::tempdir().unwrap();
        let (judge, _) = judge("http://127.0.0.1:9", dir.path());
        let judge = judge.with_modes(false, true);
        for _ in 0..FALLBACK_CALLS_PER_MINUTE {
            assert!(judge.take_fallback_budget(600));
        }
        assert!(!judge.take_fallback_budget(659), "spent within the minute");
        assert!(judge.take_fallback_budget(660), "a new minute resets it");
    }

    const FALLBACK_BODY: &str = r#"{"model":"typesafe/jev-1.13-20260917","answers":{"severity":{"type":"choice","choice":"high","probabilities":{"critical":0.05,"high":0.9,"medium":0.03,"low":0.02},"confidence":0.9},"pattern":{"type":"choice","choice":"telegram_poll_conflict","probabilities":{"telegram_poll_conflict":0.93,"external_api_4xx":0.04,"other":0.03},"confidence":0.93}},"usage":{"input_tokens":420,"output_tokens":40,"cost":0.0000176},"id":"gen-dec-f","provider":"TypeSafe"}"#;

    #[tokio::test]
    async fn fallback_applies_the_verdict_and_traces_the_row_id() {
        let base = serve("200 OK", FALLBACK_BODY, false).await;
        let dir = tempfile::tempdir().unwrap();
        let (judge, store) = judge(&base, dir.path());
        let judge = judge.with_modes(false, true);

        let verdict = judge
            .classify_fallback(
                "01ROWID",
                "mac-jane:membrane-gateway",
                "Telegram API error: Conflict: terminated by other getUpdates request",
            )
            .await;
        assert_eq!(
            verdict,
            Some((
                "high".into(),
                "telegram_poll_conflict".into(),
                "escalate".into()
            ))
        );
        let rows = store.list_traces(10).unwrap();
        assert_eq!(rows.len(), 1);
        let incumbent = rows[0].incumbent.as_ref().unwrap();
        assert_eq!(incumbent["mode"], "fallback");
        assert_eq!(incumbent["row_id"], "01ROWID");
        assert_eq!(
            incumbent["applied"]["pattern_tag"],
            "telegram_poll_conflict"
        );
    }

    #[tokio::test]
    async fn fallback_never_sends_a_model_failure_line_and_shadow_mode_does_not_apply() {
        let dir = tempfile::tempdir().unwrap();
        // Unroutable base: any network attempt would error, not return a verdict.
        let (judge, store) = judge("http://127.0.0.1:9", dir.path());
        let fallback = judge.with_modes(false, true);
        let line = "[mac-jane:model-controller][text.generate] gemini: 400 bad request";
        assert_eq!(fallback.classify_fallback("r1", "g", line).await, None);
        let rows = store.list_traces(10).unwrap();
        assert_eq!(rows[0].outcome, "skipped");
        assert_eq!(rows[0].bytes_sent, 0);

        let shadow_only = fallback.with_modes(true, false);
        assert_eq!(shadow_only.classify_fallback("r2", "g", "boom").await, None);
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
    async fn the_stored_row_never_contains_the_text_that_was_sent() {
        // The real path end to end: a line carrying a key and a home path goes
        // through the judge to a local socket, and the row that lands in the store
        // must hold none of it. (The wire-level redaction is asserted in
        // decisions-client; this pins the audit side.)
        let base = serve("200 OK", OK_BODY, false).await;
        let dir = tempfile::tempdir().unwrap();
        let (judge, store) = judge(&base, dir.path());

        judge.observe(
            "beacon",
            "boom sk-or-v1-0123456789abcdef at /Users/jaredlikes/x",
            Some(incumbent("high", "restart_guest")),
        );
        let rows = wait_for_rows(&store, 1).await;
        assert_eq!(rows[0].outcome, "ok");
        assert!(rows[0].bytes_sent > 0, "the wire body's length is recorded");
        let stored = serde_json::to_string(&rows[0]).unwrap();
        for leaked in ["sk-or-v1", "jaredlikes", "boom"] {
            assert!(!stored.contains(leaked), "`{leaked}` in {stored}");
        }
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
