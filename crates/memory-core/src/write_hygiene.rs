//! Write-side hygiene for automatic memory capture (Phase 2 M5).
//!
//! Pure helpers applied before an automatic capture (the Attend hook's
//! `memory_candidate`, deterministic operator-fact capture, external capture
//! reflexes) reaches Muninn. Explicit, operator-directed `memory.remember`
//! calls are not filtered here beyond the concept guard.

/// Automatic captures longer than this are not atomic memories; they are
/// skipped rather than truncated (a truncated fact can be a wrong fact).
/// Mirrors the LifeGraph capture lane's quality bound.
pub const AUTO_CAPTURE_MAX_CHARS: usize = 700;

/// Concept labels too generic to be an upsert key.
///
/// Every write carries `idempotent_id = "{vault}:{concept}"`, and Muninn
/// (v0.11.0+) evolves the memory pinned to that key when the content changes.
/// That is exactly right for a correction of the same fact — and silently
/// destroys a different fact that happened to get the same generic label
/// ("preference", "note", ...).
const GENERIC_CONCEPTS: &[&str] = &[
    "",
    "untitled",
    "note",
    "notes",
    "memory",
    "fact",
    "facts",
    "preference",
    "preferences",
    "decision",
    "event",
    "observation",
    "update",
    "info",
    "information",
    "reminder",
    "todo",
    "task",
    "general",
    "misc",
    "context",
    "summary",
    "user preference",
    "operator preference",
    "user fact",
];

const CONCEPT_SUBJECT_WORDS: usize = 6;

/// Return a concept safe to use as an upsert key: specific concepts pass
/// through unchanged; generic ones get a subject drawn from the content, so
/// two different facts no longer share (and overwrite) one key.
pub fn distinct_concept(concept: &str, content: &str) -> String {
    let trimmed = concept.trim();
    let normalized = trimmed.to_ascii_lowercase();
    if !GENERIC_CONCEPTS.contains(&normalized.as_str()) {
        return trimmed.to_string();
    }
    let subject: Vec<&str> = content
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .take(CONCEPT_SUBJECT_WORDS)
        .collect();
    let label = if trimmed.is_empty() { "note" } else { trimmed };
    if subject.is_empty() {
        label.to_string()
    } else {
        format!("{label}: {}", subject.join(" "))
    }
}

/// Markers of diagnostic traffic (smoke tests, canaries, routing probes) that
/// must not land in a persona's own memory, where they were being recalled into
/// unrelated operator turns (~150 times for four probe memories, 2026-09-16).
const DIAGNOSTIC_TAGS: &[&str] = &[
    "test",
    "smoke",
    "smoke-test",
    "canary",
    "probe",
    "diagnostic",
    "delete-me",
    "disposable",
];
const DIAGNOSTIC_PHRASES: &[&str] = &[
    "smoke test",
    "smoke-test",
    "routing test",
    "test probe",
    "canary",
    "this is a test",
    "test capture",
    "probe engram",
    "ignore this",
];

/// Whether an automatic capture is diagnostic traffic rather than a memory.
pub fn is_diagnostic_capture(concept: &str, content: &str, tags: &[String]) -> bool {
    if tags
        .iter()
        .any(|t| DIAGNOSTIC_TAGS.contains(&t.trim().to_ascii_lowercase().as_str()))
    {
        return true;
    }
    let haystack = format!("{concept} {content}").to_ascii_lowercase();
    DIAGNOSTIC_PHRASES
        .iter()
        .any(|phrase| haystack.contains(phrase))
}

/// Whether an automatic capture's content is too long to be one atomic memory.
pub fn exceeds_auto_capture_limit(content: &str) -> bool {
    content.chars().count() > AUTO_CAPTURE_MAX_CHARS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specific_concepts_pass_through() {
        assert_eq!(
            distinct_concept("organ-practice-schedule", "Tuesdays at 7pm"),
            "organ-practice-schedule"
        );
    }

    #[test]
    fn generic_concepts_get_a_content_subject() {
        assert_eq!(
            distinct_concept(
                "preference",
                "Prefers espresso over drip coffee, always oat milk."
            ),
            "preference: Prefers espresso over drip coffee always"
        );
        assert_eq!(
            distinct_concept("  Note ", "Sunday service moved to 10:30."),
            "Note: Sunday service moved to 10:30"
        );
        assert_eq!(
            distinct_concept("", "rehearsal notes"),
            "note: rehearsal notes"
        );
        assert_eq!(distinct_concept("memory", "   "), "memory");
        // Two different facts under one generic label no longer share a key.
        assert_ne!(
            distinct_concept("preference", "Likes window seats"),
            distinct_concept("preference", "Hates early flights")
        );
    }

    #[test]
    fn diagnostic_traffic_is_recognized() {
        assert!(is_diagnostic_capture(
            "perplexity.note: Cross-hotel routing test",
            "Cross-hotel routing test from mac-jane",
            &[]
        ));
        assert!(is_diagnostic_capture("x", "y", &["canary".into()]));
        assert!(!is_diagnostic_capture(
            "choir-rehearsal",
            "Choir rehearsal moved to Thursday; test the new anthem first.",
            &["music".into()]
        ));
    }

    #[test]
    fn auto_capture_length_bound() {
        assert!(!exceeds_auto_capture_limit(
            &"a".repeat(AUTO_CAPTURE_MAX_CHARS)
        ));
        assert!(exceeds_auto_capture_limit(
            &"a".repeat(AUTO_CAPTURE_MAX_CHARS + 1)
        ));
    }
}
