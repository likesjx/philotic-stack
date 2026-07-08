//! Sliding-window recurrence tracking for heal-queue failure patterns
//! (Autopoiesis Slice A3).
//!
//! Counts `(pattern_tag, guest_id)` occurrences inside a sliding window and
//! reports a breach when the count reaches the filing threshold. State is
//! **in-memory only**: a dispatcher restart resets every window, which is
//! acceptable — a genuinely recurring pattern re-accumulates within one
//! window, and the hotel-side open-item dedup absorbs any double filing.
//!
//! The clock is injected (`now` in Unix seconds) so tests run without
//! wall-clock time.

use ansible_mesh_core::heal_queue::cap_evidence_lines;
use std::collections::{HashMap, VecDeque};

/// Default occurrences within the window that trigger a filing.
pub const DEFAULT_FILING_THRESHOLD: u32 = 5;
/// Default sliding-window length in seconds (30 minutes).
pub const DEFAULT_FILING_WINDOW_SECS: u64 = 1800;
/// Env override for the filing threshold.
pub const ENV_FILING_THRESHOLD: &str = "PHILOTIC_HEAL_FILING_THRESHOLD";
/// Env override for the sliding-window length in seconds.
pub const ENV_FILING_WINDOW_SECS: &str = "PHILOTIC_HEAL_FILING_WINDOW_SECS";

/// A threshold breach: the pattern recurred `count` times within the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breach {
    /// Occurrences inside the window at breach time.
    pub count: u32,
    /// Raw log lines from the breach window, capped to the shared evidence
    /// budget (20 lines / 4KB — see `ansible_mesh_core::heal_queue`).
    pub evidence: Vec<String>,
}

/// In-memory sliding-window recurrence counter keyed by
/// `(pattern_tag, guest_id)`.
pub struct RecurrenceTracker {
    threshold: u32,
    window_secs: u64,
    events: HashMap<(String, String), VecDeque<(u64, String)>>,
}

impl RecurrenceTracker {
    pub fn new(threshold: u32, window_secs: u64) -> Self {
        Self {
            threshold: threshold.max(1),
            window_secs: window_secs.max(1),
            events: HashMap::new(),
        }
    }

    /// Build from env overrides, falling back to the defaults. Invalid or
    /// non-numeric values fall back silently.
    pub fn from_env(env: impl Fn(&str) -> Option<String>) -> Self {
        let threshold = env(ENV_FILING_THRESHOLD)
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(DEFAULT_FILING_THRESHOLD);
        let window_secs = env(ENV_FILING_WINDOW_SECS)
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_FILING_WINDOW_SECS);
        Self::new(threshold, window_secs)
    }

    pub fn window_secs(&self) -> u64 {
        self.window_secs
    }

    /// Record one occurrence at `now`. Returns `Some(Breach)` when the count
    /// inside the sliding window reaches the threshold.
    ///
    /// On breach the window for that key is cleared, so the *next* breach
    /// requires a full threshold of fresh occurrences — repeated filings for a
    /// still-recurring pattern arrive at most once per `threshold` new events
    /// (the hotel dedups them into count bumps on the open work item anyway).
    pub fn record(
        &mut self,
        pattern_tag: &str,
        guest_id: &str,
        now: u64,
        raw_line: &str,
    ) -> Option<Breach> {
        let key = (pattern_tag.to_string(), guest_id.to_string());
        let events = self.events.entry(key).or_default();
        let cutoff = now.saturating_sub(self.window_secs);
        while events.front().is_some_and(|(t, _)| *t < cutoff) {
            events.pop_front();
        }
        events.push_back((now, raw_line.to_string()));
        // Bound per-key memory even when the threshold is set very high.
        let bound = (self.threshold as usize).max(64);
        while events.len() > bound {
            events.pop_front();
        }
        if (events.len() as u32) >= self.threshold {
            let count = events.len() as u32;
            let lines: Vec<String> = events.iter().map(|(_, line)| line.clone()).collect();
            let evidence = cap_evidence_lines(&lines);
            events.clear();
            Some(Breach { count, evidence })
        } else {
            None
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::heal_queue::{MAX_HEAL_EVIDENCE_BYTES, MAX_HEAL_EVIDENCE_LINES};

    const T0: u64 = 1_750_000_000;

    #[test]
    fn breach_fires_at_threshold_within_window() {
        let mut tracker = RecurrenceTracker::new(5, 1800);
        for i in 0..4 {
            assert!(
                tracker
                    .record("connection_refused", "guest-a", T0 + i, "line")
                    .is_none(),
                "occurrence {} must not breach",
                i + 1
            );
        }
        let breach = tracker
            .record("connection_refused", "guest-a", T0 + 4, "line")
            .expect("fifth occurrence breaches");
        assert_eq!(breach.count, 5);
        assert_eq!(breach.evidence.len(), 5);
    }

    #[test]
    fn old_occurrences_slide_out_of_the_window() {
        let mut tracker = RecurrenceTracker::new(5, 1800);
        // Four occurrences early in the window …
        for i in 0..4 {
            assert!(
                tracker
                    .record("timeout", "guest-a", T0 + i, "line")
                    .is_none()
            );
        }
        // … expire before the fifth arrives: no breach, window restarts.
        assert!(
            tracker
                .record("timeout", "guest-a", T0 + 1801 + 3, "late line")
                .is_none(),
            "expired occurrences must not count toward the threshold"
        );
        // Occurrences exactly at the window edge still count.
        let mut tracker = RecurrenceTracker::new(2, 100);
        assert!(tracker.record("oom", "guest-b", T0, "a").is_none());
        let breach = tracker.record("oom", "guest-b", T0 + 100, "b");
        assert!(breach.is_some(), "edge-of-window occurrence still counts");
    }

    #[test]
    fn keys_are_isolated_by_pattern_and_guest() {
        let mut tracker = RecurrenceTracker::new(2, 1800);
        assert!(tracker.record("panic", "guest-a", T0, "a").is_none());
        assert!(tracker.record("panic", "guest-b", T0, "b").is_none());
        assert!(tracker.record("oom", "guest-a", T0, "c").is_none());
        // Only the exact (pattern, guest) pair accumulates.
        assert!(tracker.record("panic", "guest-a", T0 + 1, "d").is_some());
    }

    #[test]
    fn breach_clears_the_window_so_refire_needs_fresh_threshold() {
        let mut tracker = RecurrenceTracker::new(3, 1800);
        for i in 0..2 {
            assert!(tracker.record("panic", "guest-a", T0 + i, "line").is_none());
        }
        assert!(tracker.record("panic", "guest-a", T0 + 2, "line").is_some());
        // Immediately after a breach the count restarts from zero.
        assert!(tracker.record("panic", "guest-a", T0 + 3, "line").is_none());
        assert!(tracker.record("panic", "guest-a", T0 + 4, "line").is_none());
        assert!(tracker.record("panic", "guest-a", T0 + 5, "line").is_some());
    }

    #[test]
    fn breach_evidence_is_capped() {
        let mut tracker = RecurrenceTracker::new(30, 1800);
        let mut breach = None;
        for i in 0..30 {
            breach = tracker.record("panic", "guest-a", T0 + i, &format!("{i:0>300}"));
        }
        let breach = breach.expect("threshold reached");
        assert_eq!(breach.count, 30);
        assert!(breach.evidence.len() <= MAX_HEAL_EVIDENCE_LINES);
        assert!(breach.evidence.iter().map(|l| l.len()).sum::<usize>() <= MAX_HEAL_EVIDENCE_BYTES);
        // Newest lines survive.
        assert_eq!(breach.evidence.last().unwrap(), &format!("{:0>300}", 29));
    }

    #[test]
    fn env_overrides_with_fallback_on_garbage() {
        let tracker = RecurrenceTracker::from_env(|key| match key {
            ENV_FILING_THRESHOLD => Some("2".to_string()),
            ENV_FILING_WINDOW_SECS => Some("600".to_string()),
            _ => None,
        });
        assert_eq!(tracker.threshold, 2);
        assert_eq!(tracker.window_secs(), 600);

        let tracker = RecurrenceTracker::from_env(|key| match key {
            ENV_FILING_THRESHOLD => Some("not-a-number".to_string()),
            _ => None,
        });
        assert_eq!(tracker.threshold, DEFAULT_FILING_THRESHOLD);
        assert_eq!(tracker.window_secs(), DEFAULT_FILING_WINDOW_SECS);

        // Zero threshold clamps to 1 (never divide the loop into no-ops).
        let tracker = RecurrenceTracker::new(0, 0);
        assert_eq!(tracker.threshold, 1);
        assert_eq!(tracker.window_secs(), 1);
    }
}
