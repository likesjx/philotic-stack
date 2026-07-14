//! LifeGraph auto-capture lane (epic slice E2): the counterpart to the
//! auto-recall lane in `memory_integration.rs`.
//!
//! When a completed turn carries a `memory_candidate` that passes the quality
//! gate, a conservative classifier decides whether the candidate is ALSO a
//! lived-fact for the LifeGraph (OpenLoop / Commitment / Goal / Habit / Event
//! / Decision). Qualifying candidates are forked — not moved — into a
//! fire-and-forget `life.observe` task toward the LifeGraph runner, entering
//! the graph as `validation_state=proposed` nodes with per-agent provenance.
//! The Muninn continuity lane (the Attend hook in `turn_loop.rs`) is untouched
//! and keeps receiving every candidate it receives today.
//!
//! Design mirrors the auto-recall lane (PR #152):
//! - same route resolution (`resolve_tool_route`, gated on the `life_graph`
//!   class binding) with the same node/role fallbacks,
//! - a sentinel turn id ([`LIFE_AUTOCAPTURE_TURN_ID`]) so the runner's
//!   `datasource_response` is intercepted harmlessly instead of being routed
//!   as a tool result,
//! - fire-and-forget emission that never blocks or fails a turn.
//!
//! Declared as a `#[path]` child module of `runtime` so private
//! `AgentRuntime` fields stay accessible.

use super::*;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};

/// Sentinel turn id carried on auto-capture dispatches and echoed back on the
/// runner's `datasource_response`. Distinguishes capture acks from
/// model-initiated `life.observe` tool results (which carry a real turn id).
pub(super) const LIFE_AUTOCAPTURE_TURN_ID: &str = "life-autocapture";

/// Default max captures admitted per turn (env: `PHILOTIC_LIFE_AUTOCAPTURE_MAX_PER_TURN`).
pub(super) const LIFE_AUTOCAPTURE_MAX_PER_TURN: u32 = 3;

/// Default max captures admitted per session per UTC day
/// (env: `PHILOTIC_LIFE_AUTOCAPTURE_MAX_PER_DAY`).
pub(super) const LIFE_AUTOCAPTURE_MAX_PER_DAY: u32 = 20;

/// Capacity of the per-session LRU of recent capture fingerprints.
pub(super) const LIFE_AUTOCAPTURE_FINGERPRINT_LRU: usize = 64;

/// Quality gate: candidate content must be at least this many chars.
/// Mirrors the memory-candidate policy contract (atomic, 24-700 chars).
const LIFE_AUTOCAPTURE_MIN_CONTENT_CHARS: usize = 24;

/// Quality gate: candidate content must be at most this many chars.
const LIFE_AUTOCAPTURE_MAX_CONTENT_CHARS: usize = 700;

/// Quality gate: candidate concept slug must stay short.
const LIFE_AUTOCAPTURE_MAX_CONCEPT_CHARS: usize = 120;

/// Quality gate: candidates carrying more tags than this are noise.
const LIFE_AUTOCAPTURE_MAX_TAGS: usize = 10;

/// Confidence assigned when the model did not attach one to the candidate.
const LIFE_AUTOCAPTURE_DEFAULT_CONFIDENCE: f64 = 0.6;

/// Source reliability for agent-inferred observations.
const LIFE_AUTOCAPTURE_SOURCE_RELIABILITY: f64 = 0.6;

pub(super) fn life_autocapture_disabled_value(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

/// Kill switch: `PHILOTIC_DISABLE_LIFE_AUTOCAPTURE=1` disables the whole lane.
pub(super) fn life_autocapture_disabled() -> bool {
    life_autocapture_disabled_value(
        std::env::var("PHILOTIC_DISABLE_LIFE_AUTOCAPTURE")
            .ok()
            .as_deref(),
    )
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or(default)
}

pub(super) fn life_autocapture_max_per_turn() -> u32 {
    env_u32(
        "PHILOTIC_LIFE_AUTOCAPTURE_MAX_PER_TURN",
        LIFE_AUTOCAPTURE_MAX_PER_TURN,
    )
}

pub(super) fn life_autocapture_max_per_day() -> u32 {
    env_u32(
        "PHILOTIC_LIFE_AUTOCAPTURE_MAX_PER_DAY",
        LIFE_AUTOCAPTURE_MAX_PER_DAY,
    )
}

// ---------------------------------------------------------------------------
// Lived-fact classifier
// ---------------------------------------------------------------------------

/// LifeGraph labels the auto-capture lane is allowed to propose. All are
/// members of the runner's `KNOWN_LABELS` whitelist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LivedFactLabel {
    OpenLoop,
    Commitment,
    Goal,
    Habit,
    Event,
    Decision,
}

impl LivedFactLabel {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            LivedFactLabel::OpenLoop => "OpenLoop",
            LivedFactLabel::Commitment => "Commitment",
            LivedFactLabel::Goal => "Goal",
            LivedFactLabel::Habit => "Habit",
            LivedFactLabel::Event => "Event",
            LivedFactLabel::Decision => "Decision",
        }
    }

    fn id_slug(&self) -> &'static str {
        match self {
            LivedFactLabel::OpenLoop => "open-loop",
            LivedFactLabel::Commitment => "commitment",
            LivedFactLabel::Goal => "goal",
            LivedFactLabel::Habit => "habit",
            LivedFactLabel::Event => "event",
            LivedFactLabel::Decision => "decision",
        }
    }
}

/// Word-boundary containment (split at non-alphanumerics) so e.g. "goal"
/// never matches inside "goalkeeper" and "event" never matches "prevent".
fn has_word(text: &str, word: &str) -> bool {
    text.split(|ch: char| !ch.is_ascii_alphanumeric())
        .any(|segment| segment.eq_ignore_ascii_case(word))
}

/// Multiword phrase containment over a pre-lowercased haystack. Phrases carry
/// enough shape that substring matching is safe.
fn has_phrase(lower_text: &str, phrase: &str) -> bool {
    lower_text.contains(phrase)
}

struct LabelSignals {
    label: LivedFactLabel,
    /// Matched word-boundary style against concept and tags (slug vocabulary).
    slug_words: &'static [&'static str],
    /// Matched as lowercase phrases against the content.
    content_phrases: &'static [&'static str],
}

/// Conservative keyword+shape vocabulary. Under-capture is preferred over
/// graph spam: signals are explicit lived-fact markers, not general chatter.
const LABEL_SIGNALS: [LabelSignals; 6] = [
    LabelSignals {
        label: LivedFactLabel::OpenLoop,
        slug_words: &["todo", "openloop"],
        content_phrases: &[
            "open loop",
            "still needs",
            "waiting on",
            "blocked on",
            "remains unresolved",
            "left open",
            "follow up on",
            "needs follow-up",
            "unfinished",
        ],
    },
    LabelSignals {
        label: LivedFactLabel::Commitment,
        slug_words: &["commitment", "promise"],
        content_phrases: &[
            "committed to",
            "promised to",
            "agreed to",
            "will deliver",
            "owes",
        ],
    },
    LabelSignals {
        label: LivedFactLabel::Goal,
        slug_words: &["goal", "objective", "aspiration"],
        content_phrases: &[
            "goal is",
            "aims to",
            "aiming to",
            "working toward",
            "wants to achieve",
        ],
    },
    LabelSignals {
        label: LivedFactLabel::Habit,
        slug_words: &["habit", "routine", "ritual"],
        content_phrases: &[
            "every morning",
            "every evening",
            "every day",
            "every week",
            "each morning",
            "each week",
            "daily practice",
            "weekly cadence",
        ],
    },
    LabelSignals {
        label: LivedFactLabel::Event,
        slug_words: &["event"],
        content_phrases: &["took place", "happened on", "occurred on", "met with"],
    },
    LabelSignals {
        label: LivedFactLabel::Decision,
        slug_words: &["decision"],
        content_phrases: &[
            "decided to",
            "decision was",
            "chose to",
            "opted for",
            "opted to",
            "settled on",
        ],
    },
];

/// Quality gate for auto-capture, enforcing the memory-candidate policy shape
/// (atomic: short slug concept, 24-700 char content, at most 10 tags).
/// Candidates failing the gate stay Muninn-bound decisions for the Attend
/// hook — they never reach the graph through this lane.
pub(super) fn candidate_passes_quality_gate(candidate: &MemoryCandidate) -> bool {
    let concept_len = candidate.concept.trim().chars().count();
    let content_len = candidate.content.trim().chars().count();
    concept_len > 0
        && concept_len <= LIFE_AUTOCAPTURE_MAX_CONCEPT_CHARS
        && (LIFE_AUTOCAPTURE_MIN_CONTENT_CHARS..=LIFE_AUTOCAPTURE_MAX_CONTENT_CHARS)
            .contains(&content_len)
        && candidate.tags.len() <= LIFE_AUTOCAPTURE_MAX_TAGS
}

/// Classify whether a Muninn-bound memory candidate is ALSO a lived-fact for
/// the LifeGraph.
///
/// Conservative by construction: a candidate maps to a label only when
/// exactly ONE label's signal vocabulary matches. Zero matches means the
/// candidate is not a lived fact (technical/preference/continuity memory);
/// two or more matches means it is ambiguous. Both cases return `None` and
/// the candidate flows to Muninn only — never both-spam nor graph-spam.
pub(super) fn classify_lived_fact(candidate: &MemoryCandidate) -> Option<LivedFactLabel> {
    let content_lower = candidate.content.to_lowercase();
    let slug_corpus = {
        let mut corpus = candidate.concept.to_lowercase();
        for tag in &candidate.tags {
            corpus.push(' ');
            corpus.push_str(&tag.to_lowercase());
        }
        corpus
    };

    let mut matched: Option<LivedFactLabel> = None;
    for signals in &LABEL_SIGNALS {
        let hit = signals
            .slug_words
            .iter()
            .any(|word| has_word(&slug_corpus, word))
            || signals
                .content_phrases
                .iter()
                .any(|phrase| has_phrase(&content_lower, phrase));
        if hit {
            if matched.is_some() {
                // Ambiguous — two labels claim this candidate.
                return None;
            }
            matched = Some(signals.label);
        }
    }
    matched
}

// ---------------------------------------------------------------------------
// Dedup + budgets
// ---------------------------------------------------------------------------

/// Why a candidate was NOT captured. Every skip is debug-logged with its
/// reason so the lane is observable without being noisy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CaptureSkip {
    Duplicate,
    TurnBudgetExhausted,
    DailyBudgetExhausted,
}

impl CaptureSkip {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            CaptureSkip::Duplicate => "duplicate_fingerprint",
            CaptureSkip::TurnBudgetExhausted => "turn_budget_exhausted",
            CaptureSkip::DailyBudgetExhausted => "daily_budget_exhausted",
        }
    }
}

/// Normalized content fingerprint: lowercase alphanumeric tokens of
/// concept + content, whitespace/punctuation-insensitive. Stable across
/// cosmetic rephrasings of the same fact within a session.
pub(super) fn capture_fingerprint(candidate: &MemoryCandidate) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for source in [&candidate.concept, &candidate.content] {
        for token in source
            .split(|ch: char| !ch.is_ascii_alphanumeric())
            .filter(|token| !token.is_empty())
        {
            token.to_ascii_lowercase().hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// FNV-1a 64-bit. Node identity must survive process restarts *and* toolchain
/// upgrades, so it cannot ride `DefaultHasher` (stable only within one build).
fn fnv1a_64(hasher: &mut u64, bytes: &[u8]) {
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    for byte in bytes {
        *hasher ^= u64::from(*byte);
        *hasher = hasher.wrapping_mul(FNV_PRIME);
    }
}

/// Content-derived node identity for auto-captured lived facts: FNV-1a over
/// label + observing agent + the same normalized concept/content tokens as
/// [`capture_fingerprint`]. The same fact re-observed in a later turn, session,
/// or process produces the same node id, so the runner's `MERGE` upserts the
/// existing node instead of minting a duplicate. Scoped per agent so one
/// agent's re-observation never clobbers another's provenance envelope.
pub(super) fn stable_capture_node_id(
    label: LivedFactLabel,
    agent_id: &str,
    candidate: &MemoryCandidate,
) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    let mut hash = FNV_OFFSET;
    fnv1a_64(&mut hash, label.as_str().as_bytes());
    fnv1a_64(&mut hash, b"\x1f");
    fnv1a_64(&mut hash, agent_id.as_bytes());
    for source in [&candidate.concept, &candidate.content] {
        for token in source
            .split(|ch: char| !ch.is_ascii_alphanumeric())
            .filter(|token| !token.is_empty())
        {
            fnv1a_64(&mut hash, b"\x1f");
            fnv1a_64(&mut hash, token.to_ascii_lowercase().as_bytes());
        }
    }
    format!("life:{}:{hash:016x}", label.id_slug())
}

#[derive(Debug, Default)]
struct SessionCaptureState {
    /// LRU of recently captured fingerprints (front = oldest).
    recent_fingerprints: VecDeque<u64>,
    /// UTC day (unix secs / 86400) the daily counter belongs to.
    day_epoch: u64,
    captures_today: u32,
    /// Turn the per-turn counter belongs to.
    last_turn_id: String,
    captures_this_turn: u32,
}

/// Per-session dedup + budget ledger for the auto-capture lane. Live-only
/// (never checkpointed), mirroring the prefetch-dispatched flag: a process
/// restart resets dedup/budget state, which only risks a benign re-observe —
/// node ids are content-derived ([`stable_capture_node_id`]), so the runner's
/// MERGE upserts the same node instead of minting a duplicate.
#[derive(Debug, Default)]
pub(super) struct LifeCaptureLedger {
    sessions: HashMap<String, SessionCaptureState>,
}

impl LifeCaptureLedger {
    /// Admit or reject one capture. On admit, the fingerprint is recorded and
    /// budgets are charged. Dedup is checked before budgets so replayed facts
    /// never consume budget.
    pub(super) fn try_admit(
        &mut self,
        session_id: &str,
        turn_id: &str,
        fingerprint: u64,
        now_unix_secs: u64,
        max_per_turn: u32,
        max_per_day: u32,
    ) -> Result<(), CaptureSkip> {
        let state = self.sessions.entry(session_id.to_string()).or_default();

        if state.recent_fingerprints.contains(&fingerprint) {
            return Err(CaptureSkip::Duplicate);
        }

        let day_epoch = now_unix_secs / 86_400;
        if state.day_epoch != day_epoch {
            state.day_epoch = day_epoch;
            state.captures_today = 0;
        }
        if state.last_turn_id != turn_id {
            state.last_turn_id = turn_id.to_string();
            state.captures_this_turn = 0;
        }

        if state.captures_this_turn >= max_per_turn {
            return Err(CaptureSkip::TurnBudgetExhausted);
        }
        if state.captures_today >= max_per_day {
            return Err(CaptureSkip::DailyBudgetExhausted);
        }

        state.captures_this_turn += 1;
        state.captures_today += 1;
        state.recent_fingerprints.push_back(fingerprint);
        while state.recent_fingerprints.len() > LIFE_AUTOCAPTURE_FINGERPRINT_LRU {
            state.recent_fingerprints.pop_front();
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// life.observe task construction
// ---------------------------------------------------------------------------

/// OWNS edge toward the observing agent's canonical domain Role node, when
/// the agent is in the operator-decided steward map (same map the
/// auto-recall lane uses for its ranking lens, and the SCOPED_TO structural
/// anchor uses for its target). Unknown agents get no edges — never guessed.
///
/// The target id is resolved through
/// `data_memorygraphrag::zoning::canonical_role_node_id_for_agent` rather
/// than reconstructed from the domain slug: a domain slug and its seeded
/// `role_node_id` can diverge (e.g. domain `"architect"` anchors to
/// `life:role:ai_architect`, not `life:role:architect`; domain `"human"`
/// anchors to bare `"human"`, not `life:role:human`), and reconstructing
/// would fork a parallel Role node invisible to recall ranking.
pub(super) fn life_autocapture_edges(agent_id: &str) -> Vec<Value> {
    data_memorygraphrag::zoning::canonical_role_node_id_for_agent(agent_id, None)
        .map(|role_node_id| {
            vec![serde_json::json!({
                "rel_type": "OWNS",
                "target_id": role_node_id,
            })]
        })
        .unwrap_or_default()
}

/// Build the fire-and-forget `life.observe` task JSON for one captured
/// candidate. `arguments` satisfies the runner's `LifeObserveInput` contract;
/// the sentinel turn id rides the return route so the ack is intercepted as a
/// lane response instead of a tool result.
#[allow(clippy::too_many_arguments)]
pub(super) fn life_autocapture_task_json(
    agent_id: &str,
    session_id: &str,
    origin_turn_id: &str,
    chat_id: &str,
    local_node: &str,
    label: LivedFactLabel,
    candidate: &MemoryCandidate,
    observed_role: Option<&str>,
) -> Value {
    let now_iso = chrono::Utc::now().to_rfc3339();
    // Observation/evidence ids stay unique per observation; node identity is
    // content-derived so re-observations MERGE instead of forking new nodes.
    let suffix = Uuid::new_v4().simple().to_string();
    let node_id = stable_capture_node_id(label, agent_id, candidate);
    let confidence = candidate
        .confidence
        .unwrap_or(LIFE_AUTOCAPTURE_DEFAULT_CONFIDENCE)
        .clamp(0.0, 1.0);

    let arguments = serde_json::json!({
        // Per-agent provenance: observed_by is the canonical agent identity.
        "observed_by": agent_id,
        "observed_role": observed_role,
        "observation_id": format!("obs:{suffix}"),
        "evidence": {
            "packet_id": format!("evidence:{suffix}"),
            "claim_ref": {
                "id": node_id,
                "label": label.as_str(),
                "datasource": "life-graph"
            },
            "claim_summary": candidate.content,
            "source_refs": [{
                "source_id": format!("agent:{agent_id}"),
                "source_kind": "agent_inference",
                "reliability": {
                    "score": LIFE_AUTOCAPTURE_SOURCE_RELIABILITY,
                    "basis": "agent_inferred"
                }
            }],
            "passage_refs": [],
            "confidence": confidence,
            "validation_state": "proposed",
            "observed_at": now_iso,
            "source_reliability": LIFE_AUTOCAPTURE_SOURCE_RELIABILITY,
            "conflict_ids": [],
            "adjudication_status": "not_needed",
            "metadata": {
                "route": "philote_life_autocapture",
                "session_id": session_id,
                "origin_turn_id": origin_turn_id,
                "chat_id": chat_id,
                "agent_id": agent_id,
                "concept": candidate.concept,
                "tags": candidate.tags,
            }
        },
        "proposed_graph_refs": [],
        "edges": life_autocapture_edges(agent_id),
    });

    serde_json::json!({
        "action": "execute_tool",
        "tool_name": "life.observe",
        "arguments": arguments,
        "reply_to": local_node,
        "reply_role": "agent",
        "reply_guest_id": agent_id,
        "session_id": session_id,
        "turn_id": LIFE_AUTOCAPTURE_TURN_ID,
        "chat_id": "",
    })
}

impl AgentRuntime {
    /// Capture fork entry point, called from `deliver_text_reply` while the
    /// turn is still active. This is a FORK, not a move: the caller's Attend
    /// hook still writes the same candidate to Muninn afterwards.
    ///
    /// Fire-and-forget: every failure path degrades to "no capture" with a
    /// debug log — a turn is never blocked, failed, or slowed by this lane.
    pub(super) async fn maybe_autocapture_life_fact(
        &mut self,
        session_id: &str,
        candidate: Option<&MemoryCandidate>,
    ) {
        let Some(candidate) = candidate else {
            return;
        };
        if life_autocapture_disabled() {
            debug!(
                session_id = %session_id,
                "LifeGraph auto-capture skipped: lane disabled via PHILOTIC_DISABLE_LIFE_AUTOCAPTURE"
            );
            return;
        }
        if !candidate_passes_quality_gate(candidate) {
            debug!(
                session_id = %session_id,
                concept = %candidate.concept,
                "LifeGraph auto-capture skipped: candidate failed the quality gate"
            );
            return;
        }
        let Some(label) = classify_lived_fact(candidate) else {
            debug!(
                session_id = %session_id,
                concept = %candidate.concept,
                "LifeGraph auto-capture skipped: not a (unambiguous) lived fact — Muninn only"
            );
            return;
        };

        // Gated on the life_graph class binding like auto-recall: no
        // life.observe route bound for this profile = no capture lane.
        let route = self.sessions.get(session_id).and_then(|state| {
            state
                .tool_is_enabled("life.observe")
                .then(|| state.resolve_tool_route("life.observe"))
                .flatten()
                .map(|route| {
                    let node = if route.target_node.trim().is_empty() {
                        life_graph_runner_node_id()
                    } else {
                        route.target_node.clone()
                    };
                    let role = if route.target_role.trim().is_empty() {
                        "life-graph-runner".to_string()
                    } else {
                        route.target_role.clone()
                    };
                    (node, role)
                })
        });
        let Some((target_node, target_role)) = route else {
            debug!(
                session_id = %session_id,
                "LifeGraph auto-capture skipped: no life.observe route bound for this profile"
            );
            return;
        };

        let (origin_turn_id, chat_id, observed_role) = {
            let state = self.sessions.get(session_id);
            let (turn_id, chat_id) = state
                .and_then(|s| s.active_turn.as_ref())
                .map(|turn| (turn.turn_id.clone(), turn.chat_id.clone()))
                .unwrap_or_default();
            let observed_role = state
                .and_then(|s| {
                    s.role_activation
                        .as_ref()
                        .map(|activation| activation.role_name.clone())
                })
                .or_else(|| self.role_name.clone());
            (turn_id, chat_id, observed_role)
        };

        let fingerprint = capture_fingerprint(candidate);
        if let Err(skip) = self.life_capture_ledger.try_admit(
            session_id,
            &origin_turn_id,
            fingerprint,
            unix_ts_now(),
            life_autocapture_max_per_turn(),
            life_autocapture_max_per_day(),
        ) {
            debug!(
                session_id = %session_id,
                concept = %candidate.concept,
                reason = skip.as_str(),
                "LifeGraph auto-capture skipped"
            );
            return;
        }

        let task_json = life_autocapture_task_json(
            &self.agent_id,
            session_id,
            &origin_turn_id,
            &chat_id,
            &local_node_id(),
            label,
            candidate,
            observed_role.as_deref(),
        );
        let node_id = task_json
            .pointer("/arguments/evidence/claim_ref/id")
            .and_then(Value::as_str)
            .unwrap_or("life:unknown")
            .to_string();

        if let Err(err) = self
            .ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node,
                target_role,
                target_guest_id: None,
                task_json: task_json.to_string(),
            })
            .await
        {
            debug!(
                session_id = %session_id,
                error = %err,
                "LifeGraph auto-capture emit failed (lane degrades to Muninn-only)"
            );
            return;
        }

        info!(
            session_id = %session_id,
            label = label.as_str(),
            node_id = %node_id,
            concept = %candidate.concept,
            "LifeGraph auto-capture: lived-fact candidate forked to the graph as proposed"
        );
        let _ = self
            .emit_turn_event(
                session_id,
                "life_autocapture",
                Some(format!("{} → {node_id}", label.as_str())),
            )
            .await;
    }

    /// Absorb a `life.observe` auto-capture `datasource_response` (routed here
    /// by the sentinel turn id). Pure observability — success and failure are
    /// both just logged, never routed as tool results and never failing turns.
    pub(super) fn handle_life_autocapture_response(&mut self, task: &InboundTaskPayload) {
        let session_id = task.session_id.as_deref().unwrap_or("");
        if let Some(error) = task.error.as_ref() {
            debug!(
                session_id = %session_id,
                error = %error.message,
                "LifeGraph auto-capture write failed on the runner (non-fatal)"
            );
            return;
        }
        debug!(
            session_id = %session_id,
            "LifeGraph auto-capture write acknowledged by the runner"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(concept: &str, content: &str, tags: &[&str]) -> MemoryCandidate {
        MemoryCandidate {
            concept: concept.to_string(),
            content: content.to_string(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            confidence: None,
        }
    }

    // ── Classifier matrix ──────────────────────────────────────────────────

    #[test]
    fn classifier_maps_each_label() {
        let cases = [
            (
                candidate(
                    "passport renewal",
                    "The passport renewal still needs attention before the August trip.",
                    &["travel"],
                ),
                LivedFactLabel::OpenLoop,
            ),
            (
                candidate(
                    "weekly report promise",
                    "Jared committed to sending the board report every Friday this quarter.",
                    &[],
                ),
                LivedFactLabel::Commitment,
            ),
            (
                candidate(
                    "fitness goal",
                    "Jared is working toward rowing a full marathon distance by December.",
                    &[],
                ),
                LivedFactLabel::Goal,
            ),
            (
                candidate(
                    "morning rowing habit",
                    "Jared rows on the erg every morning before checking messages.",
                    &[],
                ),
                LivedFactLabel::Habit,
            ),
            (
                candidate(
                    "team offsite",
                    "The team offsite took place in Austin with the whole platform group.",
                    &[],
                ),
                LivedFactLabel::Event,
            ),
            (
                candidate(
                    "decision: sqlite over postgres",
                    "We decided to keep SQLite as the hotel store instead of Postgres.",
                    &[],
                ),
                LivedFactLabel::Decision,
            ),
        ];

        for (cand, expected) in cases {
            assert_eq!(
                classify_lived_fact(&cand),
                Some(expected),
                "candidate {:?} should classify as {:?}",
                cand.concept,
                expected
            );
        }
    }

    #[test]
    fn classifier_matches_on_tags_too() {
        let cand = candidate(
            "erg cadence",
            "Row on the erg before breakfast, tracked in the training log.",
            &["habit", "fitness"],
        );
        assert_eq!(classify_lived_fact(&cand), Some(LivedFactLabel::Habit));
    }

    #[test]
    fn classifier_skips_ambiguous_candidates() {
        // Decision + Habit both match → ambiguous → Muninn only.
        let cand = candidate(
            "new practice",
            "Jared decided to build a rowing habit with sessions every week.",
            &[],
        );
        assert_eq!(classify_lived_fact(&cand), None);
    }

    #[test]
    fn classifier_skips_non_lived_facts() {
        // Preference/technical continuity memory — zero lived-fact signals.
        let cand = candidate(
            "terminal preference",
            "The operator prefers dark mode with a monospace font in all terminals.",
            &["preference"],
        );
        assert_eq!(classify_lived_fact(&cand), None);
    }

    #[test]
    fn classifier_uses_word_boundaries_on_slugs() {
        // "event" must not fire inside "prevention", "goal" not inside "goalkeeper".
        let cand = candidate(
            "goalkeeper eventual prevention",
            "Notes about the goalkeeper and eventual prevention strategies here.",
            &[],
        );
        assert_eq!(classify_lived_fact(&cand), None);
    }

    // ── Quality gate ───────────────────────────────────────────────────────

    #[test]
    fn quality_gate_enforces_policy_shape() {
        // Healthy candidate passes.
        assert!(candidate_passes_quality_gate(&candidate(
            "rowing habit",
            "Jared rows on the erg every morning before checking messages.",
            &["habit"],
        )));
        // Too-short content (not atomic-fact sized) fails.
        assert!(!candidate_passes_quality_gate(&candidate(
            "short",
            "too short",
            &[],
        )));
        // Oversized content fails.
        assert!(!candidate_passes_quality_gate(&candidate(
            "long",
            &"x".repeat(701),
            &[],
        )));
        // Empty concept fails.
        assert!(!candidate_passes_quality_gate(&candidate(
            "  ",
            "Some perfectly reasonable content that is long enough.",
            &[],
        )));
        // Tag spam fails.
        let tags: Vec<String> = (0..11).map(|i| format!("tag{i}")).collect();
        let tag_refs: Vec<&str> = tags.iter().map(String::as_str).collect();
        assert!(!candidate_passes_quality_gate(&candidate(
            "tag spam",
            "Some perfectly reasonable content that is long enough.",
            &tag_refs,
        )));
    }

    // ── Fingerprint + dedup + budgets ──────────────────────────────────────

    #[test]
    fn fingerprint_is_normalization_stable() {
        let a = candidate(
            "rowing habit",
            "Jared rows every morning before breakfast.",
            &[],
        );
        let b = candidate(
            "Rowing   Habit",
            "  jared ROWS, every morning — before breakfast!  ",
            &[],
        );
        let c = candidate(
            "rowing habit",
            "Jared rows every evening before dinner.",
            &[],
        );
        assert_eq!(capture_fingerprint(&a), capture_fingerprint(&b));
        assert_ne!(capture_fingerprint(&a), capture_fingerprint(&c));
    }

    #[test]
    fn stable_node_id_survives_rephrasing_but_splits_on_label_agent_content() {
        let a = candidate(
            "rowing habit",
            "Jared rows every morning before breakfast.",
            &[],
        );
        let b = candidate(
            "Rowing   Habit",
            "  jared ROWS, every morning — before breakfast!  ",
            &[],
        );
        let c = candidate(
            "rowing habit",
            "Jared rows every evening before dinner.",
            &[],
        );
        let id_a = stable_capture_node_id(LivedFactLabel::Habit, "agent-beacon-01", &a);
        // Cosmetic rephrasing → same node.
        assert_eq!(
            id_a,
            stable_capture_node_id(LivedFactLabel::Habit, "agent-beacon-01", &b)
        );
        // Different content, label, or observing agent → different node.
        assert_ne!(
            id_a,
            stable_capture_node_id(LivedFactLabel::Habit, "agent-beacon-01", &c)
        );
        assert_ne!(
            id_a,
            stable_capture_node_id(LivedFactLabel::Event, "agent-beacon-01", &a)
        );
        assert_ne!(
            id_a,
            stable_capture_node_id(LivedFactLabel::Habit, "agent-astrid-01", &a)
        );
        assert!(id_a.starts_with("life:habit:"));
    }

    #[test]
    fn stable_node_id_is_a_fixed_value_across_builds() {
        // FNV-1a is toolchain-independent; pin one vector so an accidental
        // hasher swap (which would silently re-fork every captured node)
        // fails loudly here.
        let cand = candidate("anchor", "pinned regression vector for node identity.", &[]);
        assert_eq!(
            stable_capture_node_id(LivedFactLabel::Decision, "agent-test-01", &cand),
            format!(
                "life:decision:{:016x}",
                expected_fnv_for_pinned_vector()
            )
        );
    }

    fn expected_fnv_for_pinned_vector() -> u64 {
        // Independent re-derivation of the same FNV-1a stream.
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |bytes: &[u8]| {
            for byte in bytes {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(FNV_PRIME);
            }
        };
        eat(b"Decision");
        eat(b"\x1f");
        eat(b"agent-test-01");
        for token in [
            "anchor", "pinned", "regression", "vector", "for", "node", "identity",
        ] {
            eat(b"\x1f");
            eat(token.as_bytes());
        }
        hash
    }

    #[test]
    fn ledger_deduplicates_repeated_facts() {
        let mut ledger = LifeCaptureLedger::default();
        let now = 1_800_000_000;
        assert!(ledger.try_admit("s1", "t1", 42, now, 3, 20).is_ok());
        assert_eq!(
            ledger.try_admit("s1", "t2", 42, now, 3, 20),
            Err(CaptureSkip::Duplicate)
        );
        // Other sessions are independent.
        assert!(ledger.try_admit("s2", "t1", 42, now, 3, 20).is_ok());
    }

    #[test]
    fn ledger_enforces_turn_budget() {
        let mut ledger = LifeCaptureLedger::default();
        let now = 1_800_000_000;
        for fp in 0..3u64 {
            assert!(ledger.try_admit("s1", "t1", fp, now, 3, 20).is_ok());
        }
        assert_eq!(
            ledger.try_admit("s1", "t1", 99, now, 3, 20),
            Err(CaptureSkip::TurnBudgetExhausted)
        );
        // A new turn resets the per-turn counter.
        assert!(ledger.try_admit("s1", "t2", 99, now, 3, 20).is_ok());
    }

    #[test]
    fn ledger_enforces_daily_budget_and_resets_next_day() {
        let mut ledger = LifeCaptureLedger::default();
        let now = 1_800_000_000;
        for fp in 0..4u64 {
            // Spread over turns so the turn budget never interferes.
            let turn = format!("t{fp}");
            assert!(ledger.try_admit("s1", &turn, fp, now, 3, 4).is_ok());
        }
        assert_eq!(
            ledger.try_admit("s1", "t9", 100, now, 3, 4),
            Err(CaptureSkip::DailyBudgetExhausted)
        );
        // Next UTC day the budget resets.
        assert!(
            ledger
                .try_admit("s1", "t10", 101, now + 86_400, 3, 4)
                .is_ok()
        );
    }

    #[test]
    fn ledger_duplicates_do_not_consume_budget() {
        let mut ledger = LifeCaptureLedger::default();
        let now = 1_800_000_000;
        assert!(ledger.try_admit("s1", "t1", 7, now, 3, 2).is_ok());
        for _ in 0..5 {
            assert_eq!(
                ledger.try_admit("s1", "t1", 7, now, 3, 2),
                Err(CaptureSkip::Duplicate)
            );
        }
        // Only one daily slot consumed — a fresh fact still fits.
        assert!(ledger.try_admit("s1", "t1", 8, now, 3, 2).is_ok());
    }

    #[test]
    fn ledger_lru_evicts_oldest_fingerprints() {
        let mut ledger = LifeCaptureLedger::default();
        let now = 1_800_000_000;
        let total = (LIFE_AUTOCAPTURE_FINGERPRINT_LRU + 1) as u64;
        for fp in 0..total {
            let turn = format!("t{fp}");
            assert!(
                ledger
                    .try_admit("s1", &turn, fp, now, u32::MAX, u32::MAX)
                    .is_ok()
            );
        }
        // Fingerprint 0 was evicted from the LRU, so it is admissible again.
        assert!(
            ledger
                .try_admit("s1", "t-again", 0, now, u32::MAX, u32::MAX)
                .is_ok()
        );
    }

    // ── Kill switch ────────────────────────────────────────────────────────

    #[test]
    fn kill_switch_env_gate_values() {
        assert!(life_autocapture_disabled_value(Some("1")));
        assert!(life_autocapture_disabled_value(Some("true")));
        assert!(life_autocapture_disabled_value(Some(" yes ")));
        assert!(!life_autocapture_disabled_value(Some("0")));
        assert!(!life_autocapture_disabled_value(Some("")));
        assert!(!life_autocapture_disabled_value(Some("false")));
        assert!(!life_autocapture_disabled_value(None));
    }

    // ── Task construction: contract, sentinel routing, edges, fork ────────

    #[test]
    fn autocapture_task_json_matches_runner_contract() {
        let cand = MemoryCandidate {
            concept: "decision: sqlite over postgres".into(),
            content: "We decided to keep SQLite as the hotel store instead of Postgres.".into(),
            tags: vec!["architecture".into()],
            confidence: Some(0.85),
        };
        let task = life_autocapture_task_json(
            "agent-beacon-01",
            "sess-1",
            "turn-real-1",
            "chat-9",
            "mbp-jane-aiua-01",
            LivedFactLabel::Decision,
            &cand,
            Some("chief_of_staff"),
        );

        assert_eq!(task["action"], "execute_tool");
        assert_eq!(task["tool_name"], "life.observe");
        assert_eq!(task["turn_id"], LIFE_AUTOCAPTURE_TURN_ID);
        assert_eq!(task["session_id"], "sess-1");
        assert_eq!(task["reply_to"], "mbp-jane-aiua-01");
        assert_eq!(task["reply_role"], "agent");
        assert_eq!(task["reply_guest_id"], "agent-beacon-01");

        let args = &task["arguments"];
        assert_eq!(args["observed_by"], "agent-beacon-01");
        assert_eq!(args["observed_role"], "chief_of_staff");
        assert_eq!(args["evidence"]["claim_ref"]["label"], "Decision");
        assert_eq!(args["evidence"]["validation_state"], "proposed");
        assert_eq!(args["evidence"]["confidence"], 0.85);
        assert_eq!(
            args["evidence"]["source_refs"][0]["source_kind"],
            "agent_inference"
        );
        assert_eq!(
            args["evidence"]["source_refs"][0]["source_id"],
            "agent:agent-beacon-01"
        );
        assert_eq!(
            args["evidence"]["metadata"]["route"],
            "philote_life_autocapture"
        );
        assert_eq!(
            args["evidence"]["metadata"]["origin_turn_id"],
            "turn-real-1"
        );
        assert!(
            args["evidence"]["claim_ref"]["id"]
                .as_str()
                .unwrap()
                .starts_with("life:decision:")
        );

        // The runner deserializes `arguments` as LifeObserveInput — this
        // dispatch must survive the contract, provenance intact.
        let parsed: data_memorygraphrag::LifeObserveInput =
            serde_json::from_value(args.clone()).expect("arguments parse as LifeObserveInput");
        assert_eq!(parsed.observed_by.as_deref(), Some("agent-beacon-01"));
        assert_eq!(parsed.observed_role.as_deref(), Some("chief_of_staff"));
        parsed
            .evidence
            .validate()
            .expect("evidence passes contract");
    }

    #[test]
    fn autocapture_sentinel_is_distinct_from_prefetch_sentinel() {
        // Both sentinels are intercepted in handle_datasource_response; they
        // must never shadow each other or look like a real turn id.
        assert_ne!(LIFE_AUTOCAPTURE_TURN_ID, LIFE_AUTORECALL_PREFETCH_TURN_ID);
        assert!(!LIFE_AUTOCAPTURE_TURN_ID.is_empty());
    }

    #[test]
    fn autocapture_edges_derive_owns_toward_domain_role() {
        let edges = life_autocapture_edges("agent-beacon-01");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0]["rel_type"], "OWNS");
        assert_eq!(edges[0]["target_id"], "life:role:chief-of-staff");

        let edges = life_autocapture_edges("agent-astrid-01");
        assert_eq!(edges[0]["target_id"], "life:role:librarian");

        // Discriminating cases: domain slug != role_node_id suffix. A naive
        // slug-of-domain reconstruction forks to "life:role:architect" /
        // "life:role:human" instead of the seeded canonical ids.
        let edges = life_autocapture_edges("agent-aria-01");
        assert_eq!(edges[0]["target_id"], "life:role:ai_architect");

        let edges = life_autocapture_edges("agent-coach-01");
        assert_eq!(edges[0]["target_id"], "human");

        // Unknown agents get no edges — never guessed.
        assert!(life_autocapture_edges("agent-unknown-01").is_empty());
    }

    #[test]
    fn autocapture_edges_survive_cypher_compilation() {
        let cand = candidate(
            "fitness goal",
            "Jared is working toward rowing a marathon distance by December.",
            &[],
        );
        let task = life_autocapture_task_json(
            "agent-beacon-01",
            "sess-1",
            "turn-1",
            "chat-1",
            "node-1",
            LivedFactLabel::Goal,
            &cand,
            None,
        );
        let input: data_memorygraphrag::LifeObserveInput =
            serde_json::from_value(task["arguments"].clone()).expect("contract parse");
        assert_eq!(input.edges.len(), 1);
        assert_eq!(input.edges[0].rel_type, "OWNS");

        let compiled = data_memorygraphrag::cypher::compile_observe_edges(&input)
            .expect("OWNS edge compiles against the living-cycle vocabulary");
        assert_eq!(compiled.len(), 1);
        assert_eq!(compiled[0].target_id, "life:role:chief-of-staff");
    }

    #[test]
    fn autocapture_is_a_fork_not_a_move() {
        // Task construction borrows the candidate: the identical candidate
        // remains available for the Muninn Attend hook afterwards.
        let cand = MemoryCandidate {
            concept: "decision: sqlite over postgres".into(),
            content: "We decided to keep SQLite as the hotel store instead of Postgres.".into(),
            tags: vec!["architecture".into()],
            confidence: Some(0.85),
        };
        let _task = life_autocapture_task_json(
            "agent-beacon-01",
            "sess-1",
            "turn-1",
            "chat-1",
            "node-1",
            LivedFactLabel::Decision,
            &cand,
            None,
        );
        // Candidate is untouched — Muninn receives exactly what it does today.
        assert_eq!(cand.concept, "decision: sqlite over postgres");
        assert_eq!(
            cand.content,
            "We decided to keep SQLite as the hotel store instead of Postgres."
        );
        assert_eq!(cand.tags, vec!["architecture".to_string()]);
        assert_eq!(cand.confidence, Some(0.85));
    }

    #[test]
    fn default_confidence_applies_when_candidate_has_none() {
        let cand = candidate(
            "fitness goal",
            "Jared is working toward rowing a marathon distance by December.",
            &[],
        );
        let task = life_autocapture_task_json(
            "agent-jane-01",
            "sess-1",
            "turn-1",
            "chat-1",
            "node-1",
            LivedFactLabel::Goal,
            &cand,
            None,
        );
        assert_eq!(
            task["arguments"]["evidence"]["confidence"],
            LIFE_AUTOCAPTURE_DEFAULT_CONFIDENCE
        );
    }

    #[test]
    fn budget_env_parsing_falls_back_to_defaults() {
        assert_eq!(env_u32("PHILOTIC_TEST_NO_SUCH_VAR_XYZ", 7), 7);
    }
}
