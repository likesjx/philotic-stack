//! S3 — deterministic operator-fact capture: a high-precision floor under the
//! model-gated `memory_candidate` path.
//!
//! The audit found capture is entirely at the model's whim — if the model does
//! not emit a `memory_candidate`, nothing is stored. This module adds a
//! conservative, deterministic classifier that recognizes a few *explicit*
//! operator statements (an imperative "remember …", a "my X is Y" fact, a clear
//! preference, a name to go by) and proposes a capture the model missed.
//!
//! Precision over recall, deliberately. The store we are hardening must not be
//! polluted with heuristic noise, so this only fires on unambiguous patterns,
//! never on questions, and produces a *low-importance* candidate. It is a
//! floor, not the primary path: the caller uses it only when the model emitted
//! no `memory_candidate` of its own.
//!
//! The attend-hook wiring (fire the classifier when `memory_candidate` is
//! `None`, and write the result to the operator's `user_` vault through the
//! shared-write forward path so it reaches the Cortex and is fleet-recallable)
//! is the integration step, deferred until it can be live-validated that the
//! classifier does not over-capture in practice — the classifier is exercised
//! by unit tests until then.

#![allow(dead_code)]

use std::sync::LazyLock;

use regex::Regex;

/// A fact the classifier extracted from an operator turn. The caller writes it
/// at [`CapturedFact::scope`] with a `deterministic-capture` provenance tag.
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedFact {
    pub concept: String,
    pub content: String,
    /// The memory scope to store under. Operator facts/preferences are about
    /// the user, so they go to `"user"` (SharedUser → forwarded to the Cortex,
    /// fleet-visible and recallable) rather than the agent's private self vault.
    pub scope: &'static str,
    /// A durable type label for the memory.
    pub kind: &'static str,
}

/// Deliberately low — a heuristic capture should never outrank a memory the
/// model or operator asserted explicitly.
pub const DETERMINISTIC_CAPTURE_IMPORTANCE: f64 = 0.4;

/// Explicit imperative to remember something: "remember that X", "remember: X",
/// "note that X", "don't forget X", "keep in mind X". Captures the remainder.
static IMPERATIVE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\s*(?:please\s+)?(?:remember|note|keep in mind|don'?t forget)(?:\s+that|\s*[:,])?\s+(.{6,})$",
    )
    .expect("static regex")
});

/// "my <thing> is <value>" — a durable operator fact (timezone, email, etc.).
static MY_X_IS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bmy\s+([a-z][a-z0-9 _-]{1,30}?)\s+(?:is|are)\s+([^.?!]{2,60})")
        .expect("static regex")
});

/// A clear standing preference: "I prefer X", "I always X", "I never X",
/// "I like X better".
static PREFERENCE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bI\s+(prefer|always|never|can'?t stand|hate|love)\s+([^.?!]{2,60})")
        .expect("static regex")
});

/// A name to use: "call me X", "I go by X", "my name is X".
static NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:call me|I go by|my name is)\s+([A-Za-z][A-Za-z .'-]{1,40})")
        .expect("static regex")
});

fn clean(s: &str) -> String {
    s.trim().trim_end_matches(['.', ',', ';', ' ']).to_string()
}

/// Classify an operator turn into an explicit, durable fact, or `None`.
/// Conservative: returns `None` for questions and anything not matching a clear
/// pattern.
pub fn classify_operator_fact(user_text: &str) -> Option<CapturedFact> {
    let text = user_text.trim();
    // Never capture from a question — it is a request, not a stated fact.
    if text.is_empty() || text.ends_with('?') {
        return None;
    }

    // Name first — most specific.
    if let Some(caps) = NAME_RE.captures(text) {
        let name = clean(&caps[1]);
        if !name.is_empty() {
            return Some(CapturedFact {
                concept: "operator name/preferred address".into(),
                content: format!("The operator goes by {name}."),
                scope: "user",
                kind: "identity",
            });
        }
    }

    if let Some(caps) = MY_X_IS_RE.captures(text) {
        let thing = clean(&caps[1]);
        let value = clean(&caps[2]);
        if !thing.is_empty() && !value.is_empty() {
            return Some(CapturedFact {
                concept: format!("operator {thing}"),
                content: format!("The operator's {thing} is {value}."),
                scope: "user",
                kind: "fact",
            });
        }
    }

    if let Some(caps) = PREFERENCE_RE.captures(text) {
        let verb = clean(&caps[1]).to_lowercase();
        let obj = clean(&caps[2]);
        if !obj.is_empty() {
            return Some(CapturedFact {
                concept: "operator preference".into(),
                content: format!("The operator {verb} {obj}."),
                scope: "user",
                kind: "preference",
            });
        }
    }

    // Imperative last — broadest, so more specific facts win first.
    if let Some(caps) = IMPERATIVE_RE.captures(text) {
        let body = clean(&caps[1]);
        if body.len() >= 6 {
            return Some(CapturedFact {
                concept: "operator instruction to remember".into(),
                content: body,
                scope: "user",
                kind: "preference",
            });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_explicit_imperatives() {
        let f = classify_operator_fact("Remember that I deploy on Fridays only").unwrap();
        assert_eq!(f.scope, "user");
        assert!(f.content.contains("deploy on Fridays"));

        assert!(classify_operator_fact("don't forget the vps runs Memgraph on 7687").is_some());
        assert!(classify_operator_fact("Note: the Cortex is the single writer").is_some());
    }

    #[test]
    fn captures_facts_preferences_and_names() {
        let tz = classify_operator_fact("my timezone is US/Eastern").unwrap();
        assert_eq!(tz.kind, "fact");
        assert!(tz.content.to_lowercase().contains("timezone"));

        let pref = classify_operator_fact("I prefer squash merges over rebases").unwrap();
        assert_eq!(pref.kind, "preference");

        let name = classify_operator_fact("call me Jared").unwrap();
        assert_eq!(name.kind, "identity");
        assert!(name.content.contains("Jared"));
    }

    #[test]
    fn ignores_questions_and_noise() {
        // Questions are requests, not stated facts.
        assert!(classify_operator_fact("what is my timezone?").is_none());
        assert!(classify_operator_fact("can you remember that for me?").is_none());
        // Ordinary chatter with no explicit durable statement.
        assert!(classify_operator_fact("let's look at the deploy logs").is_none());
        assert!(classify_operator_fact("thanks, that worked").is_none());
        assert!(classify_operator_fact("").is_none());
    }

    #[test]
    fn everything_captured_targets_the_shared_user_scope() {
        // Operator facts must reach the shared store, not a local self vault.
        for t in [
            "remember that I like dark mode",
            "my email is x@y.com",
            "I never force-push to main",
            "call me Jay",
        ] {
            assert_eq!(classify_operator_fact(t).unwrap().scope, "user", "{t}");
        }
    }
}
