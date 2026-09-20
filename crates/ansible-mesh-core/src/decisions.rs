//! Decisions envelope — provider-neutral typed judgments (slice D0).
//!
//! A *decision* is a third kind of model capability next to generate and embed:
//! the caller sends a piece of `state` plus typed questions (`noul` yes/no,
//! `choice` one-of-N, `score` on an ordered scale) and gets back typed answers
//! with probabilities and no free text. TypeSafe's Jev ("System One") is the
//! first provider, reachable natively or through OpenRouter's alpha endpoint;
//! this module defines the canonical request/response types and two pure wire
//! adapters so neither transport leaks into callers. See
//! `docs/architecture/DECISIONS_MODEL_PROPOSAL.md`.
//!
//! # Authority
//!
//! This module is pure: no network, no clock, no storage. Callers own the HTTP
//! hop, the latency measurement, the thresholds and the deterministic fallback.
//!
//! # Invariants baked into the types
//!
//! - The envelope carries **distributions, not verdicts**. There is no
//!   threshold or "decided" field; the act / confirm / escalate mapping lives in
//!   call-site code and is calibrated per question.
//! - Questions and `choice` options are **ordered arrays** with unique keys.
//!   `serde_json` has no `preserve_order` here, so the wire adapters serialize
//!   from typed structs (never through `serde_json::Value`) to keep the order.
//! - Every failure is a [`DecisionsError`] whose meaning is "use the
//!   deterministic decision". None of them may fail a user turn.
//! - A returned choice or probability key that the request did not offer is an
//!   error, even though the vendor claims it cannot happen.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

/// Capability string for a decisions call (the `TaskKind::Decide` wire name).
pub const CAPABILITY_DECISIONS_EVALUATE: &str = "decisions.evaluate";

/// Pinned OpenRouter model for shadow sites. A moving alias would change
/// decisions silently; calibration is only meaningful per model version.
pub const PINNED_OPENROUTER_MODEL: &str = "typesafe/jev-1.13";

/// Native TypeSafe path (relative to `https://api.typesafe.ai`).
pub const NATIVE_PATH: &str = "/v1/systemone";
/// OpenRouter alpha path (relative to `https://openrouter.ai`).
pub const OPENROUTER_PATH: &str = "/api/alpha/decisions";

/// A `choice` question takes at most this many options (vendor limit).
pub const MAX_CHOICE_OPTIONS: usize = 255;
/// A `score` question takes 2 to this many ordered levels (vendor limit).
pub const MAX_SCORE_LEVELS: usize = 10;
const MIN_OPTIONS: usize = 2;
const MAX_ID_LEN: usize = 64;
const ERROR_BODY_LIMIT: usize = 200;
/// Probabilities may be rounded or top-k, so allow a little slack over 1.0.
const PROBABILITY_SUM_SLACK: f64 = 1e-3;

/// Conservative bytes-per-token used to estimate request size before sending.
/// Real tokenizers average nearer four bytes per token, so this over-estimates
/// and rejects early; the provider's 422 remains authoritative.
const BYTES_PER_TOKEN_ESTIMATE: usize = 3;

/// TypeSafe docs (`models.md`): "64k tokens per request; 32k tokens for `state`
/// plus the longest question". The second cap applies whatever the transport.
pub const STATE_PLUS_QUESTION_BUDGET_TOKENS: usize = 32_000;

// ──────────────────────────────────────────────────────────────────────────────
// Transport
// ──────────────────────────────────────────────────────────────────────────────

/// Which HTTP surface carries the request. Neither is chat completions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionsTransport {
    /// `POST https://api.typesafe.ai/v1/systemone` (bearer key, waitlisted).
    Native,
    /// `POST https://openrouter.ai/api/alpha/decisions` (ordinary OpenRouter key).
    OpenRouter,
}

impl DecisionsTransport {
    /// Request path, relative to the transport's base URL.
    pub fn path(self) -> &'static str {
        match self {
            Self::Native => NATIVE_PATH,
            Self::OpenRouter => OPENROUTER_PATH,
        }
    }

    /// Whole-request context budget in tokens (native 64 k, OpenRouter 32 k).
    /// Separately, `state` plus the longest question is capped at
    /// [`STATE_PLUS_QUESTION_BUDGET_TOKENS`] on both.
    pub fn context_budget_tokens(self) -> usize {
        match self {
            Self::Native => 64_000,
            Self::OpenRouter => 32_000,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::OpenRouter => "openrouter",
        }
    }
}

/// Map a provider-neutral model name onto the transport's model slug.
///
/// - OpenRouter: a name that already contains `/` (`typesafe/jev-1.13`,
///   `~typesafe/jev-latest`) passes through; `jev-latest` becomes the tilde
///   alias `~typesafe/jev-latest`; anything else is prefixed with `typesafe/`.
///   The bare `typesafe/jev-latest` does **not** exist (verified HTTP 400).
/// - Native: any `~typesafe/` or `typesafe/` prefix is stripped.
pub fn wire_model(transport: DecisionsTransport, model: &str) -> String {
    match transport {
        DecisionsTransport::OpenRouter => {
            if model.contains('/') {
                model.to_string()
            } else if model == "jev-latest" {
                "~typesafe/jev-latest".to_string()
            } else {
                format!("typesafe/{model}")
            }
        }
        DecisionsTransport::Native => model
            .strip_prefix("~typesafe/")
            .or_else(|| model.strip_prefix("typesafe/"))
            .unwrap_or(model)
            .to_string(),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Errors
// ──────────────────────────────────────────────────────────────────────────────

/// Why a decision could not be produced. Every class means "use the
/// deterministic decision"; none may fail a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionsErrorClass {
    /// Provider unreachable or 5xx.
    Unavailable,
    /// Deadline exceeded.
    Timeout,
    /// 429 or 529 (overload). Back off.
    RateLimited,
    /// Our request was rejected or failed local validation (400/422).
    InvalidRequest,
    /// 401/403 — missing, invalid or unauthorised key.
    Auth,
    /// The provider answered but the answer is unusable (malformed, unknown
    /// choice, missing or extra answers, out-of-range probability).
    InvalidResponse,
}

impl DecisionsErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Timeout => "timeout",
            Self::RateLimited => "rate_limited",
            Self::InvalidRequest => "invalid_request",
            Self::Auth => "auth",
            Self::InvalidResponse => "invalid_response",
        }
    }

    /// Whether a bounded retry can plausibly help. Decisions still fall back
    /// deterministically when the retry budget is spent.
    pub fn retryable(self) -> bool {
        matches!(self, Self::Unavailable | Self::Timeout | Self::RateLimited)
    }
}

impl fmt::Display for DecisionsErrorClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("decisions {class}: {message}")]
pub struct DecisionsError {
    pub class: DecisionsErrorClass,
    pub message: String,
}

impl DecisionsError {
    pub fn new(class: DecisionsErrorClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
        }
    }

    fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(DecisionsErrorClass::InvalidRequest, message)
    }

    fn invalid_response(message: impl Into<String>) -> Self {
        Self::new(DecisionsErrorClass::InvalidResponse, message)
    }
}

/// Classify a non-success HTTP status. The body is truncated and may contain
/// the vendor's offending-field details (never our `state`).
pub fn classify_http_status(status: u16, body: &str) -> DecisionsError {
    let class = match status {
        401 | 403 => DecisionsErrorClass::Auth,
        400 | 404 | 422 => DecisionsErrorClass::InvalidRequest,
        408 | 504 => DecisionsErrorClass::Timeout,
        429 | 529 => DecisionsErrorClass::RateLimited,
        _ => DecisionsErrorClass::Unavailable,
    };
    DecisionsError::new(class, format!("HTTP {status}: {}", truncate(body)))
}

fn truncate(text: &str) -> String {
    let trimmed = text.trim();
    match trimmed.char_indices().nth(ERROR_BODY_LIMIT) {
        Some((cut, _)) => format!("{}…", &trimmed[..cut]),
        None => trimmed.to_string(),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Canonical request
// ──────────────────────────────────────────────────────────────────────────────

/// One selectable option (`choice`) or ordered level (`score`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionOption {
    /// Stable machine key. Answers refer to this key, never to the description.
    pub key: String,
    /// What the option means, shown to the model. `None` sends `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl DecisionOption {
    pub fn new(key: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            description: Some(description.into()),
        }
    }

    /// The string sent for a `score` level: the description, else the key.
    fn level_text(&self) -> &str {
        self.description.as_deref().unwrap_or(&self.key)
    }
}

/// The typed part of a question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QuestionSpec {
    /// Yes/no. The answer is P(true).
    Noul {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        when_true: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        when_false: Option<String>,
    },
    /// Exactly one of 2..=255 options, in caller order.
    Choice { options: Vec<DecisionOption> },
    /// Position on 2..=10 ordered levels, lowest first. The order is the
    /// semantics, so this is an array, never an object.
    Score { levels: Vec<DecisionOption> },
}

impl QuestionSpec {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Noul { .. } => "noul",
            Self::Choice { .. } => "choice",
            Self::Score { .. } => "score",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionQuestion {
    /// Unique within the request; the key of the matching answer.
    pub id: String,
    pub instructions: String,
    #[serde(flatten)]
    pub spec: QuestionSpec,
}

/// State plus ordered typed questions for one call site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionsRequest {
    /// Call-site id (`heal.classify`). Keys shadow rows and traces. Required.
    pub site: String,
    /// What is being judged: a string, object or array. Text only.
    pub state: Value,
    /// Ordered, unique ids.
    pub questions: Vec<DecisionQuestion>,
}

impl DecisionsRequest {
    fn state_bytes(&self) -> usize {
        match &self.state {
            Value::String(text) => text.len(),
            other => other.to_string().len(),
        }
    }

    /// Rough token count of everything the provider will read.
    pub fn estimated_tokens(&self) -> usize {
        let questions: usize = self.questions.iter().map(question_bytes).sum();
        (self.state_bytes() + questions).div_ceil(BYTES_PER_TOKEN_ESTIMATE)
    }

    /// Rough token count of `state` plus the single longest question — the
    /// quantity the vendor caps at 32 k regardless of the total request budget.
    pub fn estimated_state_plus_longest_question_tokens(&self) -> usize {
        let longest = self.questions.iter().map(question_bytes).max().unwrap_or(0);
        (self.state_bytes() + longest).div_ceil(BYTES_PER_TOKEN_ESTIMATE)
    }

    /// Reject a request the provider would reject, before spending a network hop.
    pub fn validate(&self, transport: DecisionsTransport) -> Result<(), DecisionsError> {
        validate_site(&self.site)?;
        if self.state.is_null() {
            return Err(DecisionsError::invalid_request("state must not be null"));
        }
        if self.questions.is_empty() {
            return Err(DecisionsError::invalid_request(
                "at least one question is required",
            ));
        }
        let mut ids = BTreeSet::new();
        for question in &self.questions {
            validate_id("question id", &question.id)?;
            if !ids.insert(question.id.as_str()) {
                return Err(DecisionsError::invalid_request(format!(
                    "duplicate question id `{}`",
                    question.id
                )));
            }
            if question.instructions.trim().is_empty() {
                return Err(DecisionsError::invalid_request(format!(
                    "question `{}` has empty instructions",
                    question.id
                )));
            }
            validate_spec(&question.id, &question.spec)?;
        }
        let state_and_question = self.estimated_state_plus_longest_question_tokens();
        if state_and_question > STATE_PLUS_QUESTION_BUDGET_TOKENS {
            return Err(DecisionsError::invalid_request(format!(
                "state plus the longest question is about {state_and_question} tokens, \
                 over the {STATE_PLUS_QUESTION_BUDGET_TOKENS}-token cap"
            )));
        }
        let estimate = self.estimated_tokens();
        let budget = transport.context_budget_tokens();
        if estimate > budget {
            return Err(DecisionsError::invalid_request(format!(
                "request is about {estimate} tokens, over the {budget}-token {} budget",
                transport.as_str()
            )));
        }
        Ok(())
    }
}

/// Bytes a question contributes to the provider's input.
fn question_bytes(question: &DecisionQuestion) -> usize {
    let spec = match &question.spec {
        QuestionSpec::Noul {
            when_true,
            when_false,
        } => when_true.as_deref().map_or(0, str::len) + when_false.as_deref().map_or(0, str::len),
        QuestionSpec::Choice { options } | QuestionSpec::Score { levels: options } => options
            .iter()
            .map(|o| o.key.len() + o.description.as_deref().map_or(0, str::len))
            .sum(),
    };
    question.id.len() + question.instructions.len() + spec
}

fn validate_site(site: &str) -> Result<(), DecisionsError> {
    let ok = !site.is_empty()
        && site.len() <= MAX_ID_LEN
        && site.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        });
    if ok {
        Ok(())
    } else {
        Err(DecisionsError::invalid_request(format!(
            "site `{site}` must be 1..={MAX_ID_LEN} chars of [a-z0-9._-]"
        )))
    }
}

fn validate_id(what: &str, id: &str) -> Result<(), DecisionsError> {
    if id.is_empty() || id.len() > MAX_ID_LEN || id.chars().any(char::is_control) {
        return Err(DecisionsError::invalid_request(format!(
            "{what} `{id}` must be 1..={MAX_ID_LEN} chars with no control characters"
        )));
    }
    Ok(())
}

fn validate_spec(question_id: &str, spec: &QuestionSpec) -> Result<(), DecisionsError> {
    match spec {
        QuestionSpec::Noul { .. } => Ok(()),
        QuestionSpec::Choice { options } => {
            validate_options(question_id, "options", options, MAX_CHOICE_OPTIONS)
        }
        QuestionSpec::Score { levels } => {
            validate_options(question_id, "levels", levels, MAX_SCORE_LEVELS)?;
            // Levels travel as plain strings and come back by index, so two
            // levels with the same text would be indistinguishable.
            let mut texts = BTreeSet::new();
            for level in levels {
                if !texts.insert(level.level_text()) {
                    return Err(DecisionsError::invalid_request(format!(
                        "question `{question_id}` has two levels with the same text `{}`",
                        level.level_text()
                    )));
                }
            }
            Ok(())
        }
    }
}

fn validate_options(
    question_id: &str,
    what: &str,
    options: &[DecisionOption],
    max: usize,
) -> Result<(), DecisionsError> {
    if options.len() < MIN_OPTIONS || options.len() > max {
        return Err(DecisionsError::invalid_request(format!(
            "question `{question_id}` needs {MIN_OPTIONS}..={max} {what}, got {}",
            options.len()
        )));
    }
    let mut keys = BTreeSet::new();
    for option in options {
        validate_id("option key", &option.key)?;
        if !keys.insert(option.key.as_str()) {
            return Err(DecisionsError::invalid_request(format!(
                "question `{question_id}` repeats key `{}`",
                option.key
            )));
        }
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Canonical response
// ──────────────────────────────────────────────────────────────────────────────

/// One typed answer. Probabilities are keyed by the request's own option or
/// level keys, whatever the wire used.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DecisionAnswer {
    /// P(true) in `0.0..=1.0`.
    Noul { noul: f64 },
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    /// `score` is the probability-weighted level index (0 = lowest level).
    Score {
        score: f64,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionsResult {
    pub site: String,
    /// One answer per requested question, keyed by question id.
    pub answers: BTreeMap<String, DecisionAnswer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct DecisionsUsage {
    pub input_tokens: u64,
    /// Non-zero but free for Jev; record it, never assume zero.
    pub output_tokens: u64,
    /// OpenRouter only (`usage.cost`, input-priced). `None` on native.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// Provenance for one call, for `decision_traces` and calibration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionsTrace {
    /// As reported by the provider (`TypeSafe` on OpenRouter), else `typesafe`.
    pub provider: String,
    /// The **resolved** model the provider answered with
    /// (`typesafe/jev-1.13-20260917`), not the alias requested. Calibration is
    /// per model version.
    pub model: String,
    pub transport: DecisionsTransport,
    /// Measured by the caller around the HTTP hop.
    pub latency_ms: u64,
    pub usage: DecisionsUsage,
    /// OpenRouter generation id (`gen-dec-…`), when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// A `score` legend did not echo the levels we sent, so the answer was
    /// mapped by legend text or position. Count these separately: the
    /// `score` value is in the provider's index space and should not feed
    /// calibration until the mismatch is understood.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub legend_mismatch: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionsOutcome {
    pub result: DecisionsResult,
    pub trace: DecisionsTrace,
}

/// A parsed provider response, before the caller adds transport and latency.
#[derive(Debug, Clone, PartialEq)]
pub struct WireOutcome {
    pub resolved_model: String,
    pub provider: Option<String>,
    pub request_id: Option<String>,
    /// A `score` legend differed from the levels we sent.
    pub legend_mismatch: bool,
    pub usage: DecisionsUsage,
    pub result: DecisionsResult,
}

impl WireOutcome {
    pub fn into_outcome(self, transport: DecisionsTransport, latency_ms: u64) -> DecisionsOutcome {
        DecisionsOutcome {
            result: self.result,
            trace: DecisionsTrace {
                provider: self.provider.unwrap_or_else(|| "typesafe".to_string()),
                model: self.resolved_model,
                transport,
                latency_ms,
                usage: self.usage,
                request_id: self.request_id,
                legend_mismatch: self.legend_mismatch,
            },
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Wire request
// ──────────────────────────────────────────────────────────────────────────────

/// A JSON object whose entries are emitted in the order given.
struct OrderedMap<'a, V>(Vec<(&'a str, V)>);

impl<V: Serialize> Serialize for OrderedMap<'_, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

#[derive(Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    state: &'a Value,
    questions: OrderedMap<'a, WireQuestion<'a>>,
}

#[derive(Serialize)]
struct WireNoulCriteria<'a> {
    #[serde(rename = "true", skip_serializing_if = "Option::is_none")]
    when_true: Option<&'a str>,
    #[serde(rename = "false", skip_serializing_if = "Option::is_none")]
    when_false: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum WireQuestion<'a> {
    Noul {
        instructions: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<WireNoulCriteria<'a>>,
    },
    Choice {
        instructions: &'a str,
        criteria: OrderedMap<'a, Option<&'a str>>,
    },
    Score {
        instructions: &'a str,
        criteria: Vec<&'a str>,
    },
}

fn wire_question(transport: DecisionsTransport, question: &DecisionQuestion) -> WireQuestion<'_> {
    let instructions = question.instructions.as_str();
    match &question.spec {
        QuestionSpec::Noul {
            when_true,
            when_false,
        } => {
            let (when_true, when_false) = (when_true.as_deref(), when_false.as_deref());
            let criteria = match (transport, when_true, when_false) {
                (_, None, None) => None,
                // OpenRouter requires both sides whenever criteria is present;
                // a missing side becomes a neutral empty description.
                (DecisionsTransport::OpenRouter, t, f) => Some(WireNoulCriteria {
                    when_true: Some(t.unwrap_or("")),
                    when_false: Some(f.unwrap_or("")),
                }),
                (DecisionsTransport::Native, t, f) => Some(WireNoulCriteria {
                    when_true: t,
                    when_false: f,
                }),
            };
            WireQuestion::Noul {
                instructions,
                criteria,
            }
        }
        QuestionSpec::Choice { options } => WireQuestion::Choice {
            instructions,
            criteria: OrderedMap(
                options
                    .iter()
                    .map(|o| (o.key.as_str(), o.description.as_deref()))
                    .collect(),
            ),
        },
        QuestionSpec::Score { levels } => WireQuestion::Score {
            instructions,
            criteria: levels.iter().map(DecisionOption::level_text).collect(),
        },
    }
}

/// Validate `request` and render the JSON body for `transport`.
///
/// `model` is provider-neutral (`jev-1.13`, `jev-latest`) or already a wire
/// slug; see [`wire_model`]. Question and option order is preserved.
pub fn build_wire_request(
    transport: DecisionsTransport,
    request: &DecisionsRequest,
    model: &str,
) -> Result<String, DecisionsError> {
    request.validate(transport)?;
    let model = wire_model(transport, model);
    let wire = WireRequest {
        model: &model,
        state: &request.state,
        questions: OrderedMap(
            request
                .questions
                .iter()
                .map(|q| (q.id.as_str(), wire_question(transport, q)))
                .collect(),
        ),
    };
    serde_json::to_string(&wire)
        .map_err(|err| DecisionsError::invalid_request(format!("cannot encode request: {err}")))
}

// ──────────────────────────────────────────────────────────────────────────────
// Wire response
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct WireResponse {
    model: String,
    answers: BTreeMap<String, Value>,
    #[serde(default)]
    usage: WireUsage,
    // OpenRouter adds these on top of the native shape.
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    provider: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cost: Option<f64>,
}

#[derive(Deserialize)]
struct WireNoulAnswer {
    noul: f64,
}

#[derive(Deserialize)]
struct WireChoiceAnswer {
    choice: String,
    probabilities: BTreeMap<String, f64>,
    confidence: f64,
}

#[derive(Deserialize)]
struct WireScoreAnswer {
    score: f64,
    legend: BTreeMap<String, String>,
    probabilities: BTreeMap<String, f64>,
    confidence: f64,
}

/// Parse and validate a provider response against the request that produced it.
///
/// Both transports share this parser (OpenRouter's body is a superset). The
/// request drives the parse: each answer is read as the type its question
/// declared, every requested id must be answered, and nothing extra may appear.
pub fn parse_wire_response(
    request: &DecisionsRequest,
    body: &str,
) -> Result<WireOutcome, DecisionsError> {
    let wire: WireResponse = serde_json::from_str(body)
        .map_err(|err| DecisionsError::invalid_response(format!("malformed response: {err}")))?;

    let requested: BTreeSet<&str> = request.questions.iter().map(|q| q.id.as_str()).collect();
    if let Some(extra) = wire
        .answers
        .keys()
        .find(|id| !requested.contains(id.as_str()))
    {
        return Err(DecisionsError::invalid_response(format!(
            "answer for unrequested question `{extra}`"
        )));
    }

    let mut answers = BTreeMap::new();
    let mut legend_mismatch = false;
    for question in &request.questions {
        let raw = wire.answers.get(&question.id).ok_or_else(|| {
            DecisionsError::invalid_response(format!("no answer for question `{}`", question.id))
        })?;
        if let Some(tag) = raw.get("type").and_then(Value::as_str) {
            if tag != question.spec.type_name() {
                return Err(DecisionsError::invalid_response(format!(
                    "question `{}` is {} but the answer is {tag}",
                    question.id,
                    question.spec.type_name()
                )));
            }
        }
        let (answer, mismatch) = parse_answer(question, raw)?;
        legend_mismatch |= mismatch;
        answers.insert(question.id.clone(), answer);
    }

    Ok(WireOutcome {
        resolved_model: wire.model,
        provider: wire.provider,
        request_id: wire.id,
        legend_mismatch,
        usage: DecisionsUsage {
            input_tokens: wire.usage.input_tokens,
            output_tokens: wire.usage.output_tokens,
            cost_usd: wire.usage.cost,
        },
        result: DecisionsResult {
            site: request.site.clone(),
            answers,
        },
    })
}

/// Parse one answer. The flag is true when a `score` legend did not match the
/// levels we sent (see the legend policy below).
fn parse_answer(
    question: &DecisionQuestion,
    raw: &Value,
) -> Result<(DecisionAnswer, bool), DecisionsError> {
    let id = question.id.as_str();
    let malformed = |err: serde_json::Error| {
        DecisionsError::invalid_response(format!("answer `{id}` is malformed: {err}"))
    };
    match &question.spec {
        QuestionSpec::Noul { .. } => {
            let answer: WireNoulAnswer = serde_json::from_value(raw.clone()).map_err(malformed)?;
            check_unit(id, "noul", answer.noul)?;
            Ok((DecisionAnswer::Noul { noul: answer.noul }, false))
        }
        QuestionSpec::Choice { options } => {
            let answer: WireChoiceAnswer =
                serde_json::from_value(raw.clone()).map_err(malformed)?;
            let keys: BTreeSet<&str> = options.iter().map(|o| o.key.as_str()).collect();
            if !keys.contains(answer.choice.as_str()) {
                return Err(DecisionsError::invalid_response(format!(
                    "answer `{id}` chose `{}`, which was not offered",
                    answer.choice
                )));
            }
            if let Some(unknown) = answer
                .probabilities
                .keys()
                .find(|k| !keys.contains(k.as_str()))
            {
                return Err(DecisionsError::invalid_response(format!(
                    "answer `{id}` gives a probability for `{unknown}`, which was not offered"
                )));
            }
            check_distribution(id, &answer.probabilities, answer.confidence)?;
            Ok((
                DecisionAnswer::Choice {
                    choice: answer.choice,
                    probabilities: answer.probabilities,
                    confidence: answer.confidence,
                },
                false,
            ))
        }
        QuestionSpec::Score { levels } => {
            let answer: WireScoreAnswer = serde_json::from_value(raw.clone()).map_err(malformed)?;
            if answer.legend.len() != levels.len() || answer.probabilities.len() > levels.len() {
                return Err(DecisionsError::invalid_response(format!(
                    "answer `{id}` has {} legend entries and {} probabilities for {} levels",
                    answer.legend.len(),
                    answer.probabilities.len(),
                    levels.len()
                )));
            }
            // The wire speaks in level indices; callers speak in level keys.
            // The legend says what each index means, so its text is the
            // authority when it names one of our levels. If it does not (the
            // vendor trimmed or rewrote the text, or reordered the levels) we
            // fall back to position and flag the answer, rather than failing:
            // in shadow mode a strict check would turn every score answer into
            // an error indistinguishable from "the judge disagrees".
            let mut position = Vec::with_capacity(levels.len());
            let mut legend_mismatch = false;
            for index in 0..levels.len() {
                let text = answer.legend.get(&index.to_string()).ok_or_else(|| {
                    DecisionsError::invalid_response(format!(
                        "answer `{id}` legend has no entry for level index {index}"
                    ))
                })?;
                let by_text: Vec<usize> = (0..levels.len())
                    .filter(|&i| levels[i].level_text() == text)
                    .collect();
                match by_text.as_slice() {
                    [i] if *i == index => position.push(index),
                    [i] => {
                        legend_mismatch = true;
                        position.push(*i);
                    }
                    _ => {
                        legend_mismatch = true;
                        position.push(index);
                    }
                }
            }
            if position.iter().collect::<BTreeSet<_>>().len() != levels.len() {
                return Err(DecisionsError::invalid_response(format!(
                    "answer `{id}` legend maps two indices to the same level"
                )));
            }
            let mut probabilities = BTreeMap::new();
            for (index, probability) in &answer.probabilities {
                let key = index
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| position.get(i))
                    .map(|&i| levels[i].key.clone())
                    .ok_or_else(|| {
                        DecisionsError::invalid_response(format!(
                            "answer `{id}` gives a probability for level index `{index}`, which does not exist"
                        ))
                    })?;
                probabilities.insert(key, *probability);
            }
            check_distribution(id, &probabilities, answer.confidence)?;
            let max_index = (levels.len() - 1) as f64;
            if !answer.score.is_finite()
                || answer.score < -PROBABILITY_SUM_SLACK
                || answer.score > max_index + PROBABILITY_SUM_SLACK
            {
                return Err(DecisionsError::invalid_response(format!(
                    "answer `{id}` score {} is outside 0..={max_index}",
                    answer.score
                )));
            }
            Ok((
                DecisionAnswer::Score {
                    score: answer.score,
                    probabilities,
                    confidence: answer.confidence,
                },
                legend_mismatch,
            ))
        }
    }
}

fn check_unit(id: &str, what: &str, value: f64) -> Result<(), DecisionsError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(DecisionsError::invalid_response(format!(
            "answer `{id}` {what} {value} is outside 0..=1"
        )))
    }
}

fn check_distribution(
    id: &str,
    probabilities: &BTreeMap<String, f64>,
    confidence: f64,
) -> Result<(), DecisionsError> {
    check_unit(id, "confidence", confidence)?;
    for (key, probability) in probabilities {
        check_unit(id, &format!("probability[{key}]"), *probability)?;
    }
    let sum: f64 = probabilities.values().sum();
    if sum > 1.0 + PROBABILITY_SUM_SLACK {
        return Err(DecisionsError::invalid_response(format!(
            "answer `{id}` probabilities sum to {sum}, above 1"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Recorded live 2026-09-19 (OpenRouter, `typesafe/jev-1.13`, one `noul`).
    /// The generation id is redacted; the shape and numbers are as returned.
    const RECORDED_OPENROUTER_NOUL: &str =
        include_str!("../tests/fixtures/decisions/recorded_openrouter_noul.json");
    /// Built from TypeSafe's HTTP API docs, NOT yet recorded from a live call.
    const DOCS_NATIVE_MIXED: &str =
        include_str!("../tests/fixtures/decisions/docs_native_mixed.json");
    /// The proposal's canonical request example.
    const CANONICAL_REQUEST: &str =
        include_str!("../tests/fixtures/decisions/canonical_request_heal_classify.json");

    fn urgency_request() -> DecisionsRequest {
        DecisionsRequest {
            site: "smoke.urgency".into(),
            state: json!("Help! My payouts have been failing for 3 days."),
            questions: vec![DecisionQuestion {
                id: "is_urgent".into(),
                instructions: "Does this convey urgency?".into(),
                spec: QuestionSpec::Noul {
                    when_true: Some("Explicitly time-sensitive".into()),
                    when_false: Some("No urgency expressed".into()),
                },
            }],
        }
    }

    fn canonical() -> DecisionsRequest {
        serde_json::from_str(CANONICAL_REQUEST).expect("canonical fixture parses")
    }

    fn choice_only(options: Vec<DecisionOption>) -> DecisionsRequest {
        DecisionsRequest {
            site: "t.choice".into(),
            state: json!("x"),
            questions: vec![DecisionQuestion {
                id: "q".into(),
                instructions: "pick".into(),
                spec: QuestionSpec::Choice { options },
            }],
        }
    }

    fn score_only(levels: Vec<DecisionOption>) -> DecisionsRequest {
        DecisionsRequest {
            site: "t.score".into(),
            state: json!("x"),
            questions: vec![DecisionQuestion {
                id: "q".into(),
                instructions: "rate".into(),
                spec: QuestionSpec::Score { levels },
            }],
        }
    }

    fn options(n: usize) -> Vec<DecisionOption> {
        (0..n)
            .map(|i| DecisionOption::new(format!("k{i}"), format!("option {i}")))
            .collect()
    }

    fn class_of<T: std::fmt::Debug>(result: Result<T, DecisionsError>) -> DecisionsErrorClass {
        result.expect_err("expected an error").class
    }

    // ── canonical request ────────────────────────────────────────────────────

    #[test]
    fn canonical_request_round_trips_and_validates() {
        let request = canonical();
        assert_eq!(request.site, "heal.classify");
        assert_eq!(request.questions.len(), 3);
        assert_eq!(request.questions[0].spec.type_name(), "choice");
        assert_eq!(request.questions[1].spec.type_name(), "noul");
        assert_eq!(request.questions[2].spec.type_name(), "score");
        request.validate(DecisionsTransport::Native).unwrap();
        request.validate(DecisionsTransport::OpenRouter).unwrap();

        let again: DecisionsRequest =
            serde_json::from_str(&serde_json::to_string(&request).unwrap()).unwrap();
        assert_eq!(again, request);
    }

    #[test]
    fn validation_rejects_bad_requests_before_any_network_hop() {
        let ok = urgency_request();
        for (label, mutate) in [
            (
                "empty site",
                Box::new(|r: &mut DecisionsRequest| r.site.clear())
                    as Box<dyn Fn(&mut DecisionsRequest)>,
            ),
            (
                "upper-case site",
                Box::new(|r| r.site = "Heal.Classify".into()),
            ),
            ("null state", Box::new(|r| r.state = Value::Null)),
            ("no questions", Box::new(|r| r.questions.clear())),
            (
                "empty instructions",
                Box::new(|r| r.questions[0].instructions = "  ".into()),
            ),
            ("empty question id", Box::new(|r| r.questions[0].id.clear())),
        ] {
            let mut request = ok.clone();
            mutate(&mut request);
            assert_eq!(
                class_of(request.validate(DecisionsTransport::Native)),
                DecisionsErrorClass::InvalidRequest,
                "{label}"
            );
        }
    }

    #[test]
    fn duplicate_question_ids_are_rejected() {
        let mut request = urgency_request();
        request.questions.push(request.questions[0].clone());
        assert_eq!(
            class_of(request.validate(DecisionsTransport::Native)),
            DecisionsErrorClass::InvalidRequest
        );
    }

    #[test]
    fn choice_and_score_bounds_match_the_vendor_limits() {
        assert!(choice_only(options(2))
            .validate(DecisionsTransport::Native)
            .is_ok());
        assert!(choice_only(options(MAX_CHOICE_OPTIONS))
            .validate(DecisionsTransport::Native)
            .is_ok());
        assert!(choice_only(options(1))
            .validate(DecisionsTransport::Native)
            .is_err());
        assert!(choice_only(options(MAX_CHOICE_OPTIONS + 1))
            .validate(DecisionsTransport::Native)
            .is_err());

        assert!(score_only(options(2))
            .validate(DecisionsTransport::Native)
            .is_ok());
        assert!(score_only(options(MAX_SCORE_LEVELS))
            .validate(DecisionsTransport::Native)
            .is_ok());
        assert!(score_only(options(1))
            .validate(DecisionsTransport::Native)
            .is_err());
        assert!(score_only(options(MAX_SCORE_LEVELS + 1))
            .validate(DecisionsTransport::Native)
            .is_err());
    }

    #[test]
    fn repeated_option_keys_and_indistinguishable_levels_are_rejected() {
        let dup_keys = vec![
            DecisionOption::new("a", "one"),
            DecisionOption::new("a", "two"),
        ];
        assert!(choice_only(dup_keys)
            .validate(DecisionsTransport::Native)
            .is_err());

        let same_text = vec![
            DecisionOption::new("lo", "same"),
            DecisionOption::new("hi", "same"),
        ];
        assert!(score_only(same_text)
            .validate(DecisionsTransport::Native)
            .is_err());
    }

    #[test]
    fn state_plus_longest_question_is_capped_at_32k_on_every_transport() {
        // TypeSafe: "64k tokens per request; 32k tokens for `state` plus the longest
        // question". ~150 kB is about 50 k tokens by the estimate, over the 32 k cap
        // even though it fits the native 64 k request budget.
        let mut request = urgency_request();
        request.state = json!("x".repeat(150_000));
        for transport in [DecisionsTransport::Native, DecisionsTransport::OpenRouter] {
            assert_eq!(
                class_of(request.validate(transport)),
                DecisionsErrorClass::InvalidRequest,
                "{transport:?}"
            );
        }

        // ~90 kB is about 30 k tokens: under the cap on both.
        request.state = json!("x".repeat(90_000));
        assert!(request.validate(DecisionsTransport::Native).is_ok());
        assert!(request.validate(DecisionsTransport::OpenRouter).is_ok());
    }

    #[test]
    fn whole_request_budget_differs_by_transport() {
        // 20 k tokens of state plus 12 questions of ~3 k tokens each is ~56 k in
        // total: state plus the longest question stays under 32 k, so native accepts
        // it, but OpenRouter's 32 k request budget does not.
        let mut request = urgency_request();
        request.state = json!("x".repeat(60_000));
        request.questions = (0..12)
            .map(|i| DecisionQuestion {
                id: format!("q{i}"),
                instructions: "y".repeat(9_000),
                spec: QuestionSpec::Noul {
                    when_true: None,
                    when_false: None,
                },
            })
            .collect();
        assert!(request.validate(DecisionsTransport::Native).is_ok());
        assert_eq!(
            class_of(request.validate(DecisionsTransport::OpenRouter)),
            DecisionsErrorClass::InvalidRequest
        );

        // Past 64 k in total, native refuses too.
        request.questions.extend((12..30).map(|i| DecisionQuestion {
            id: format!("q{i}"),
            instructions: "y".repeat(9_000),
            spec: QuestionSpec::Noul {
                when_true: None,
                when_false: None,
            },
        }));
        assert_eq!(
            class_of(request.validate(DecisionsTransport::Native)),
            DecisionsErrorClass::InvalidRequest
        );
    }

    // ── model slugs ──────────────────────────────────────────────────────────

    #[test]
    fn openrouter_slugs_never_use_the_nonexistent_bare_alias() {
        let or = DecisionsTransport::OpenRouter;
        assert_eq!(wire_model(or, "jev-latest"), "~typesafe/jev-latest");
        assert_eq!(wire_model(or, "jev-1.13"), "typesafe/jev-1.13");
        assert_eq!(wire_model(or, "typesafe/jev-1.13"), "typesafe/jev-1.13");
        assert_eq!(
            wire_model(or, "~typesafe/jev-latest"),
            "~typesafe/jev-latest"
        );
        assert_eq!(
            wire_model(or, PINNED_OPENROUTER_MODEL),
            PINNED_OPENROUTER_MODEL
        );
    }

    #[test]
    fn native_slugs_drop_openrouter_prefixes() {
        let native = DecisionsTransport::Native;
        assert_eq!(wire_model(native, "jev-latest"), "jev-latest");
        assert_eq!(wire_model(native, "~typesafe/jev-latest"), "jev-latest");
        assert_eq!(wire_model(native, "typesafe/jev-1.13"), "jev-1.13");
    }

    #[test]
    fn transports_have_distinct_paths_and_budgets() {
        assert_eq!(DecisionsTransport::Native.path(), "/v1/systemone");
        assert_eq!(
            DecisionsTransport::OpenRouter.path(),
            "/api/alpha/decisions"
        );
        assert!(
            DecisionsTransport::Native.context_budget_tokens()
                > DecisionsTransport::OpenRouter.context_budget_tokens()
        );
    }

    // ── wire request ─────────────────────────────────────────────────────────

    #[test]
    fn openrouter_request_matches_the_probe_that_returned_200() {
        // Byte-for-byte the body of the 2026-09-19 probe, modulo key order.
        let body = build_wire_request(
            DecisionsTransport::OpenRouter,
            &urgency_request(),
            "jev-1.13",
        )
        .unwrap();
        let sent: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            sent,
            json!({
                "model": "typesafe/jev-1.13",
                "state": "Help! My payouts have been failing for 3 days.",
                "questions": {
                    "is_urgent": {
                        "type": "noul",
                        "instructions": "Does this convey urgency?",
                        "criteria": { "true": "Explicitly time-sensitive", "false": "No urgency expressed" }
                    }
                }
            })
        );
    }

    #[test]
    fn noul_criteria_is_optional_natively_and_filled_on_openrouter() {
        let mut request = urgency_request();
        request.questions[0].spec = QuestionSpec::Noul {
            when_true: Some("only the true side".into()),
            when_false: None,
        };
        let native: Value = serde_json::from_str(
            &build_wire_request(DecisionsTransport::Native, &request, "jev-latest").unwrap(),
        )
        .unwrap();
        assert_eq!(
            native["questions"]["is_urgent"]["criteria"],
            json!({ "true": "only the true side" })
        );

        let openrouter: Value = serde_json::from_str(
            &build_wire_request(DecisionsTransport::OpenRouter, &request, "jev-latest").unwrap(),
        )
        .unwrap();
        assert_eq!(
            openrouter["questions"]["is_urgent"]["criteria"],
            json!({ "true": "only the true side", "false": "" })
        );

        request.questions[0].spec = QuestionSpec::Noul {
            when_true: None,
            when_false: None,
        };
        for transport in [DecisionsTransport::Native, DecisionsTransport::OpenRouter] {
            let body: Value = serde_json::from_str(
                &build_wire_request(transport, &request, "jev-latest").unwrap(),
            )
            .unwrap();
            assert!(
                body["questions"]["is_urgent"].get("criteria").is_none(),
                "{transport:?}"
            );
        }
    }

    #[test]
    fn wire_request_preserves_question_option_and_level_order() {
        // Alphabetical order would put `alpha` before `zeta` and `mid` before `zed`.
        let request = DecisionsRequest {
            site: "t.order".into(),
            state: json!("x"),
            questions: vec![
                DecisionQuestion {
                    id: "zz_first".into(),
                    instructions: "pick".into(),
                    spec: QuestionSpec::Choice {
                        options: vec![
                            DecisionOption::new("zeta", "z"),
                            DecisionOption::new("alpha", "a"),
                            DecisionOption::new("mid", "m"),
                        ],
                    },
                },
                DecisionQuestion {
                    id: "aa_second".into(),
                    instructions: "rate".into(),
                    spec: QuestionSpec::Score {
                        levels: vec![
                            DecisionOption::new("lo", "zzz low"),
                            DecisionOption::new("hi", "aaa high"),
                        ],
                    },
                },
            ],
        };
        for transport in [DecisionsTransport::Native, DecisionsTransport::OpenRouter] {
            let body = build_wire_request(transport, &request, "jev-latest").unwrap();
            let at = |needle: &str| {
                body.find(needle)
                    .unwrap_or_else(|| panic!("`{needle}` missing from {body}"))
            };
            assert!(
                at("\"zz_first\"") < at("\"aa_second\""),
                "question order, {transport:?}: {body}"
            );
            assert!(
                at("\"zeta\"") < at("\"alpha\"") && at("\"alpha\"") < at("\"mid\""),
                "option order, {transport:?}: {body}"
            );
            assert!(
                at("zzz low") < at("aaa high"),
                "level order, {transport:?}: {body}"
            );
        }
    }

    #[test]
    fn choice_without_description_sends_null_and_score_falls_back_to_the_key() {
        let request = DecisionsRequest {
            site: "t.null".into(),
            state: json!("x"),
            questions: vec![
                DecisionQuestion {
                    id: "c".into(),
                    instructions: "pick".into(),
                    spec: QuestionSpec::Choice {
                        options: vec![
                            DecisionOption {
                                key: "a".into(),
                                description: None,
                            },
                            DecisionOption::new("b", "bee"),
                        ],
                    },
                },
                DecisionQuestion {
                    id: "s".into(),
                    instructions: "rate".into(),
                    spec: QuestionSpec::Score {
                        levels: vec![
                            DecisionOption {
                                key: "low".into(),
                                description: None,
                            },
                            DecisionOption::new("high", "very high"),
                        ],
                    },
                },
            ],
        };
        let body: Value = serde_json::from_str(
            &build_wire_request(DecisionsTransport::Native, &request, "jev-latest").unwrap(),
        )
        .unwrap();
        assert_eq!(
            body["questions"]["c"]["criteria"],
            json!({ "a": null, "b": "bee" })
        );
        assert_eq!(
            body["questions"]["s"]["criteria"],
            json!(["low", "very high"])
        );
    }

    #[test]
    fn building_validates_first() {
        let mut request = urgency_request();
        request.questions.clear();
        assert_eq!(
            class_of(build_wire_request(
                DecisionsTransport::Native,
                &request,
                "jev-latest"
            )),
            DecisionsErrorClass::InvalidRequest
        );
    }

    // ── wire response ────────────────────────────────────────────────────────

    #[test]
    fn recorded_openrouter_noul_response_parses() {
        let outcome = parse_wire_response(&urgency_request(), RECORDED_OPENROUTER_NOUL).unwrap();
        assert_eq!(outcome.resolved_model, "typesafe/jev-1.13-20260917");
        assert_eq!(outcome.provider.as_deref(), Some("TypeSafe"));
        assert!(outcome
            .request_id
            .as_deref()
            .unwrap()
            .starts_with("gen-dec-"));
        assert_eq!(outcome.usage.input_tokens, 307);
        assert_eq!(outcome.usage.output_tokens, 23);
        assert_eq!(outcome.usage.cost_usd, Some(0.000012894));
        assert_eq!(outcome.result.site, "smoke.urgency");
        assert_eq!(
            outcome.result.answers["is_urgent"],
            DecisionAnswer::Noul { noul: 0.95 }
        );

        let done = outcome.into_outcome(DecisionsTransport::OpenRouter, 290);
        assert_eq!(done.trace.model, "typesafe/jev-1.13-20260917");
        assert_eq!(done.trace.provider, "TypeSafe");
        assert_eq!(done.trace.latency_ms, 290);
        assert_eq!(done.trace.transport, DecisionsTransport::OpenRouter);
    }

    #[test]
    fn docs_native_response_maps_score_indices_back_to_level_keys() {
        let outcome = parse_wire_response(&canonical(), DOCS_NATIVE_MIXED).unwrap();
        assert!(!outcome.legend_mismatch);
        assert_eq!(outcome.provider, None);
        assert_eq!(outcome.usage.cost_usd, None);

        match &outcome.result.answers["severity"] {
            DecisionAnswer::Choice {
                choice,
                probabilities,
                confidence,
            } => {
                assert_eq!(choice, "high");
                assert_eq!(probabilities["high"], 0.82);
                assert_eq!(probabilities["critical"], 0.11);
                assert_eq!(*confidence, 0.82);
            }
            other => panic!("severity should be a choice, got {other:?}"),
        }
        assert_eq!(
            outcome.result.answers["needs_restart"],
            DecisionAnswer::Noul { noul: 0.07 }
        );
        match &outcome.result.answers["harm"] {
            DecisionAnswer::Score {
                score,
                probabilities,
                ..
            } => {
                assert_eq!(*score, 0.4);
                // Wire indices "0" and "1" come back as the request's own level keys.
                assert_eq!(probabilities.get("none"), Some(&0.6));
                assert_eq!(probabilities.get("mild"), Some(&0.4));
                assert!(!probabilities.contains_key("0"));
            }
            other => panic!("harm should be a score, got {other:?}"),
        }
        // Without a provider field the trace still names one.
        assert_eq!(
            outcome
                .into_outcome(DecisionsTransport::Native, 118)
                .trace
                .provider,
            "typesafe"
        );
    }

    fn with_answer(answer: Value) -> String {
        json!({ "model": "m", "answers": { "q": answer }, "usage": { "input_tokens": 1, "output_tokens": 1 } }).to_string()
    }

    fn choice_request() -> DecisionsRequest {
        choice_only(vec![
            DecisionOption::new("yes", "y"),
            DecisionOption::new("no", "n"),
        ])
    }

    fn score_request() -> DecisionsRequest {
        score_only(vec![
            DecisionOption::new("lo", "low"),
            DecisionOption::new("hi", "high"),
        ])
    }

    #[test]
    fn a_choice_the_request_did_not_offer_is_an_error() {
        let body = with_answer(
            json!({ "type": "choice", "choice": "maybe", "probabilities": { "yes": 0.5 }, "confidence": 0.5 }),
        );
        assert_eq!(
            class_of(parse_wire_response(&choice_request(), &body)),
            DecisionsErrorClass::InvalidResponse
        );

        let body = with_answer(
            json!({ "type": "choice", "choice": "yes", "probabilities": { "yes": 0.5, "maybe": 0.4 }, "confidence": 0.5 }),
        );
        assert_eq!(
            class_of(parse_wire_response(&choice_request(), &body)),
            DecisionsErrorClass::InvalidResponse
        );
    }

    #[test]
    fn out_of_range_or_overfull_probabilities_are_errors() {
        for answer in [
            json!({ "type": "noul", "noul": 1.2 }),
            json!({ "type": "noul", "noul": -0.1 }),
        ] {
            let request = urgency_request_with_id("q");
            assert_eq!(
                class_of(parse_wire_response(&request, &with_answer(answer))),
                DecisionsErrorClass::InvalidResponse
            );
        }
        let overfull = with_answer(
            json!({ "type": "choice", "choice": "yes", "probabilities": { "yes": 0.7, "no": 0.7 }, "confidence": 0.7 }),
        );
        assert_eq!(
            class_of(parse_wire_response(&choice_request(), &overfull)),
            DecisionsErrorClass::InvalidResponse
        );
        let bad_confidence = with_answer(
            json!({ "type": "choice", "choice": "yes", "probabilities": { "yes": 0.7 }, "confidence": 1.5 }),
        );
        assert_eq!(
            class_of(parse_wire_response(&choice_request(), &bad_confidence)),
            DecisionsErrorClass::InvalidResponse
        );
    }

    fn urgency_request_with_id(id: &str) -> DecisionsRequest {
        let mut request = urgency_request();
        request.questions[0].id = id.into();
        request
    }

    #[test]
    fn score_legend_is_authoritative_but_a_mismatch_is_flagged_not_fatal() {
        // The legend can echo exactly what we sent (the docs example does).
        let good = with_answer(json!({
            "type": "score", "score": 0.3, "legend": { "0": "low", "1": "high" },
            "probabilities": { "0": 0.7, "1": 0.3 }, "confidence": 0.7
        }));
        let outcome = parse_wire_response(&score_request(), &good).unwrap();
        assert!(!outcome.legend_mismatch);

        // Reordered legend: the text says index 0 is our "high" level, so that is
        // where the probability goes, and the answer is flagged.
        let reordered = with_answer(json!({
            "type": "score", "score": 0.3, "legend": { "0": "high", "1": "low" },
            "probabilities": { "0": 0.7, "1": 0.3 }, "confidence": 0.7
        }));
        let outcome = parse_wire_response(&score_request(), &reordered).unwrap();
        assert!(outcome.legend_mismatch);
        match &outcome.result.answers["q"] {
            DecisionAnswer::Score { probabilities, .. } => {
                assert_eq!(probabilities["hi"], 0.7);
                assert_eq!(probabilities["lo"], 0.3);
            }
            other => panic!("expected a score, got {other:?}"),
        }
        assert!(
            outcome
                .into_outcome(DecisionsTransport::Native, 1)
                .trace
                .legend_mismatch
        );

        // Rewritten legend text (trimmed, re-cased, translated): fall back to
        // position and flag it, so a vendor-side normalisation cannot turn every
        // score answer into an error.
        let rewritten = with_answer(json!({
            "type": "score", "score": 0.3, "legend": { "0": "Low.", "1": "High." },
            "probabilities": { "0": 0.7, "1": 0.3 }, "confidence": 0.7
        }));
        let outcome = parse_wire_response(&score_request(), &rewritten).unwrap();
        assert!(outcome.legend_mismatch);
        match &outcome.result.answers["q"] {
            DecisionAnswer::Score { probabilities, .. } => {
                assert_eq!(probabilities["lo"], 0.7);
                assert_eq!(probabilities["hi"], 0.3);
            }
            other => panic!("expected a score, got {other:?}"),
        }

        // Two indices claiming the same level cannot be mapped.
        let ambiguous = with_answer(json!({
            "type": "score", "score": 0.3, "legend": { "0": "low", "1": "low" },
            "probabilities": { "0": 0.7, "1": 0.3 }, "confidence": 0.7
        }));
        assert_eq!(
            class_of(parse_wire_response(&score_request(), &ambiguous)),
            DecisionsErrorClass::InvalidResponse
        );

        let short = with_answer(json!({
            "type": "score", "score": 0.3, "legend": { "0": "low" },
            "probabilities": { "0": 1.0 }, "confidence": 0.7
        }));
        assert_eq!(
            class_of(parse_wire_response(&score_request(), &short)),
            DecisionsErrorClass::InvalidResponse
        );

        let bad_index = with_answer(json!({
            "type": "score", "score": 0.3, "legend": { "0": "low", "1": "high" },
            "probabilities": { "0": 0.5, "7": 0.5 }, "confidence": 0.7
        }));
        assert_eq!(
            class_of(parse_wire_response(&score_request(), &bad_index)),
            DecisionsErrorClass::InvalidResponse
        );

        let out_of_range = with_answer(json!({
            "type": "score", "score": 4.0, "legend": { "0": "low", "1": "high" },
            "probabilities": { "0": 0.7, "1": 0.3 }, "confidence": 0.7
        }));
        assert_eq!(
            class_of(parse_wire_response(&score_request(), &out_of_range)),
            DecisionsErrorClass::InvalidResponse
        );
    }

    #[test]
    fn missing_extra_and_mistyped_answers_are_errors() {
        let request = urgency_request();

        let missing = json!({ "model": "m", "answers": {}, "usage": {} }).to_string();
        assert_eq!(
            class_of(parse_wire_response(&request, &missing)),
            DecisionsErrorClass::InvalidResponse
        );

        let extra = json!({
            "model": "m",
            "answers": { "is_urgent": { "type": "noul", "noul": 0.5 }, "stowaway": { "type": "noul", "noul": 0.5 } },
            "usage": {}
        })
        .to_string();
        assert_eq!(
            class_of(parse_wire_response(&request, &extra)),
            DecisionsErrorClass::InvalidResponse
        );

        let mistyped = json!({
            "model": "m",
            "answers": { "is_urgent": { "type": "choice", "choice": "a", "probabilities": {}, "confidence": 0.5 } },
            "usage": {}
        })
        .to_string();
        assert_eq!(
            class_of(parse_wire_response(&request, &mistyped)),
            DecisionsErrorClass::InvalidResponse
        );
    }

    #[test]
    fn malformed_bodies_are_invalid_responses() {
        let request = urgency_request();
        assert_eq!(
            class_of(parse_wire_response(&request, "not json")),
            DecisionsErrorClass::InvalidResponse
        );
        assert_eq!(
            class_of(parse_wire_response(&request, "{}")),
            DecisionsErrorClass::InvalidResponse
        );
        // A resolved model is required: calibration is per model version.
        let no_model =
            json!({ "answers": { "is_urgent": { "type": "noul", "noul": 0.5 } } }).to_string();
        assert_eq!(
            class_of(parse_wire_response(&request, &no_model)),
            DecisionsErrorClass::InvalidResponse
        );
    }

    #[test]
    fn a_missing_type_tag_is_tolerated_when_the_shape_matches() {
        let body = with_answer(json!({ "noul": 0.25 }));
        let outcome = parse_wire_response(&urgency_request_with_id("q"), &body).unwrap();
        assert_eq!(
            outcome.result.answers["q"],
            DecisionAnswer::Noul { noul: 0.25 }
        );
    }

    // ── errors ───────────────────────────────────────────────────────────────

    #[test]
    fn http_statuses_map_to_the_error_classes_in_the_proposal() {
        for (status, class) in [
            (401, DecisionsErrorClass::Auth),
            (403, DecisionsErrorClass::Auth),
            (400, DecisionsErrorClass::InvalidRequest),
            (422, DecisionsErrorClass::InvalidRequest),
            (429, DecisionsErrorClass::RateLimited),
            (529, DecisionsErrorClass::RateLimited),
            (408, DecisionsErrorClass::Timeout),
            (504, DecisionsErrorClass::Timeout),
            (500, DecisionsErrorClass::Unavailable),
            (502, DecisionsErrorClass::Unavailable),
        ] {
            assert_eq!(
                classify_http_status(status, "").class,
                class,
                "HTTP {status}"
            );
        }
    }

    #[test]
    fn retryable_classes_are_the_transient_ones() {
        assert!(DecisionsErrorClass::RateLimited.retryable());
        assert!(DecisionsErrorClass::Timeout.retryable());
        assert!(DecisionsErrorClass::Unavailable.retryable());
        assert!(!DecisionsErrorClass::Auth.retryable());
        assert!(!DecisionsErrorClass::InvalidRequest.retryable());
        assert!(!DecisionsErrorClass::InvalidResponse.retryable());
    }

    #[test]
    fn error_bodies_are_truncated_on_a_char_boundary() {
        let body = "é".repeat(500);
        let error = classify_http_status(422, &body);
        assert!(error.message.starts_with("HTTP 422: "));
        assert!(error.message.ends_with('…'));
        assert!(error.message.chars().count() < 230);
    }

    #[test]
    fn errors_and_answers_serialize_for_the_ipc_reply() {
        let error = DecisionsError::new(DecisionsErrorClass::RateLimited, "slow down");
        let round: DecisionsError =
            serde_json::from_str(&serde_json::to_string(&error).unwrap()).unwrap();
        assert_eq!(round, error);
        assert_eq!(
            serde_json::to_value(&error).unwrap()["class"],
            "rate_limited"
        );

        let answer = DecisionAnswer::Choice {
            choice: "high".into(),
            probabilities: BTreeMap::from([("high".to_string(), 0.8)]),
            confidence: 0.8,
        };
        let value = serde_json::to_value(&answer).unwrap();
        assert_eq!(value["type"], "choice");
        assert_eq!(
            serde_json::from_value::<DecisionAnswer>(value).unwrap(),
            answer
        );
    }
}
