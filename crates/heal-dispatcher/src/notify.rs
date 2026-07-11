//! Operator-notification throttle for the self-heal circuit (finding F4).
//!
//! Two events warrant an active operator ping through the existing operator
//! gateway (EmitTask → operator-facing role → Telegram/Beacon):
//!
//!  * **A work-item filing** — a pattern recurred past the filing threshold and
//!    the hotel opened a durable `heal_work_item`. Significant on the first
//!    occurrence; the hotel already dedups re-breaches into count bumps, so the
//!    dispatcher only sees `filed=true` once per fresh item.
//!  * **A repeated single-occurrence escalate** — a pattern that classifies to
//!    `escalate` (auth failure, expired key, missing file, gated LLM restart)
//!    but never reaches the work-item threshold. One escalate is noise; the
//!    *same* pattern_tag escalating repeatedly in a window is a standing fault
//!    an operator should see.
//!
//! Both decisions share ONE hard per-tag cooldown so escalations can never spam
//! the operator. State is in-memory (a dispatcher restart resets it — the same
//! acceptable tradeoff the recurrence tracker makes) and the clock is injected
//! (`now` in Unix seconds) so the throttle is unit-testable without wall time.
//!
//! This type only makes the *decision*. The actual push is thin, best-effort
//! glue in `main` that swallows every error (including disconnect) — a failed
//! notification must never break the dispatch loop or a filing.

use std::collections::HashMap;

/// Default hard per-tag cooldown between operator pushes (1 hour).
pub const DEFAULT_NOTIFY_COOLDOWN_SECS: u64 = 3600;
/// Default number of escalates for the same tag within the window before the
/// first repeated-escalate notification fires (one escalate alone is noise).
pub const DEFAULT_ESCALATE_REPEAT_THRESHOLD: u32 = 2;
/// Default sliding window over which repeated escalates are counted (30 min).
pub const DEFAULT_ESCALATE_WINDOW_SECS: u64 = 1800;

/// Env override for the per-tag cooldown.
pub const ENV_NOTIFY_COOLDOWN_SECS: &str = "PHILOTIC_HEAL_NOTIFY_COOLDOWN_SECS";
/// Env override for the repeated-escalate threshold.
pub const ENV_ESCALATE_REPEAT_THRESHOLD: &str = "PHILOTIC_HEAL_ESCALATE_REPEAT_THRESHOLD";
/// Env override for the repeated-escalate window.
pub const ENV_ESCALATE_WINDOW_SECS: &str = "PHILOTIC_HEAL_ESCALATE_WINDOW_SECS";
/// Env override for the operator-facing role EmitTask targets.
///
/// The default (`orchestrator`) is deliberately a **fail-safe, no-subscriber**
/// role: a persona/authority role that no guest subscribes to under its bare
/// name and that is not a mesh-advertised `NodeRole` capability, so an unrouted
/// ping stays ledger-only (durable, no side effects) rather than fanning out.
/// Bare `agent` was rejected precisely because `deliver_inbound_task` with no
/// guest_id fans a task to *every* persona subscriber — a turn-storm in the
/// self-heal system.
///
/// For onward Telegram/Beacon delivery, set this to the deployment's actual
/// operator-facing routing key — the `role:{agent_id}:{role_name}` form the
/// hotel uses for role incarnations (e.g. `role:agent-beacon:orchestrator`).
/// That inbox belongs to the cognitive agent that holds the operator's live
/// session, so the ping reaches the operator the same way the Beacon heartbeat
/// cron does. Guaranteeing a chat_id-bound push without an operator session is
/// a separate follow-up (no global operator chat_id exists today).
pub const ENV_ESCALATION_ROLE: &str = "PHILOTIC_HEAL_ESCALATION_ROLE";
/// Default operator-facing role — a fail-safe no-subscriber persona role (see
/// [`ENV_ESCALATION_ROLE`]); real delivery is via the env override.
pub const DEFAULT_ESCALATION_ROLE: &str = "orchestrator";

#[derive(Default)]
struct TagState {
    /// Escalate timestamps still inside the repeat window.
    escalates: Vec<u64>,
    /// When we last pushed an operator notification for this tag.
    last_notified: Option<u64>,
}

/// In-memory per-`pattern_tag` operator-notification throttle.
pub struct EscalationNotifier {
    cooldown_secs: u64,
    escalate_window_secs: u64,
    escalate_repeat_threshold: u32,
    tags: HashMap<String, TagState>,
}

impl EscalationNotifier {
    pub fn new(
        cooldown_secs: u64,
        escalate_window_secs: u64,
        escalate_repeat_threshold: u32,
    ) -> Self {
        Self {
            // A zero cooldown would defeat the anti-spam guarantee — floor at 1s.
            cooldown_secs: cooldown_secs.max(1),
            escalate_window_secs: escalate_window_secs.max(1),
            // Threshold of 0/1 both mean "notify on the first repeat check"; keep
            // the intent explicit by flooring at 1.
            escalate_repeat_threshold: escalate_repeat_threshold.max(1),
            tags: HashMap::new(),
        }
    }

    /// Build from env overrides, falling back to defaults. Garbage values fall
    /// back silently — a bad env var must never disable the safety notification.
    pub fn from_env(env: impl Fn(&str) -> Option<String>) -> Self {
        let cooldown = env(ENV_NOTIFY_COOLDOWN_SECS)
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_NOTIFY_COOLDOWN_SECS);
        let window = env(ENV_ESCALATE_WINDOW_SECS)
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_ESCALATE_WINDOW_SECS);
        let threshold = env(ENV_ESCALATE_REPEAT_THRESHOLD)
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(DEFAULT_ESCALATE_REPEAT_THRESHOLD);
        Self::new(cooldown, window, threshold)
    }

    /// Resolve the operator-facing target role from env (best-effort default).
    pub fn escalation_role(env: impl Fn(&str) -> Option<String>) -> String {
        env(ENV_ESCALATION_ROLE)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_ESCALATION_ROLE.to_string())
    }

    /// Cooldown gate shared by both event kinds: true (and stamps `now`) when no
    /// push has fired for this tag within the cooldown.
    fn cooldown_open(&mut self, tag: &str, now: u64) -> bool {
        let state = self.tags.entry(tag.to_string()).or_default();
        let open = match state.last_notified {
            None => true,
            Some(last) => now.saturating_sub(last) >= self.cooldown_secs,
        };
        if open {
            state.last_notified = Some(now);
        }
        open
    }

    /// A fresh work-item filing warrants a notification, subject only to the
    /// per-tag cooldown. Returns true when the caller should push.
    pub fn should_notify_filing(&mut self, pattern_tag: &str, now: u64) -> bool {
        self.cooldown_open(pattern_tag, now)
    }

    /// A single `escalate` occurred for `pattern_tag`. Returns true when the
    /// caller should push: only once the same tag has escalated at least
    /// `escalate_repeat_threshold` times within the window, and only if the
    /// per-tag cooldown is open.
    pub fn should_notify_escalate(&mut self, pattern_tag: &str, now: u64) -> bool {
        // Prune + record inside the window first.
        let window = self.escalate_window_secs;
        let cutoff = now.saturating_sub(window);
        {
            let state = self.tags.entry(pattern_tag.to_string()).or_default();
            state.escalates.retain(|t| *t >= cutoff);
            state.escalates.push(now);
            // Bound per-tag memory regardless of threshold.
            let bound = (self.escalate_repeat_threshold as usize).max(64);
            if state.escalates.len() > bound {
                let overflow = state.escalates.len() - bound;
                state.escalates.drain(0..overflow);
            }
            if (state.escalates.len() as u32) < self.escalate_repeat_threshold {
                return false;
            }
        }
        // Threshold met — apply the shared cooldown gate.
        self.cooldown_open(pattern_tag, now)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_750_000_000;

    #[test]
    fn filing_notifies_once_then_throttles_within_cooldown() {
        let mut n = EscalationNotifier::new(3600, 1800, 2);
        // First filing pushes.
        assert!(n.should_notify_filing("connection_refused", T0));
        // A second filing for the same tag inside the cooldown is suppressed —
        // "invoked once per filing, throttled on repeat".
        assert!(!n.should_notify_filing("connection_refused", T0 + 60));
        assert!(!n.should_notify_filing("connection_refused", T0 + 3599));
        // After the cooldown elapses a fresh filing pushes again.
        assert!(n.should_notify_filing("connection_refused", T0 + 3600));
    }

    #[test]
    fn distinct_tags_do_not_throttle_each_other() {
        let mut n = EscalationNotifier::new(3600, 1800, 2);
        assert!(n.should_notify_filing("auth_failure", T0));
        assert!(n.should_notify_filing("api_key_expired", T0 + 1));
    }

    #[test]
    fn single_escalate_is_noise_repeated_escalate_notifies_once() {
        let mut n = EscalationNotifier::new(3600, 1800, 2);
        // One escalate alone must NOT ping the operator.
        assert!(!n.should_notify_escalate("auth_failure", T0));
        // A second escalate of the same tag within the window crosses the
        // repeat threshold → one notification.
        assert!(n.should_notify_escalate("auth_failure", T0 + 30));
        // Further escalates inside the cooldown are throttled (no spam).
        assert!(!n.should_notify_escalate("auth_failure", T0 + 60));
        assert!(!n.should_notify_escalate("auth_failure", T0 + 120));
    }

    #[test]
    fn escalates_outside_window_do_not_accumulate() {
        let mut n = EscalationNotifier::new(3600, 100, 2);
        assert!(!n.should_notify_escalate("timeout", T0));
        // Second escalate arrives after the window — the first has expired, so
        // this is again a lone escalate and must not notify.
        assert!(!n.should_notify_escalate("timeout", T0 + 101));
    }

    #[test]
    fn escalate_then_filing_share_the_cooldown() {
        let mut n = EscalationNotifier::new(3600, 1800, 2);
        // Two escalates trip the notification.
        assert!(!n.should_notify_escalate("missing_file", T0));
        assert!(n.should_notify_escalate("missing_file", T0 + 10));
        // A filing for the SAME tag inside the cooldown is suppressed — the
        // operator was just told about this pattern.
        assert!(!n.should_notify_filing("missing_file", T0 + 20));
    }

    #[test]
    fn env_overrides_with_fallback_on_garbage() {
        let n = EscalationNotifier::from_env(|k| match k {
            ENV_NOTIFY_COOLDOWN_SECS => Some("120".into()),
            ENV_ESCALATE_WINDOW_SECS => Some("300".into()),
            ENV_ESCALATE_REPEAT_THRESHOLD => Some("3".into()),
            _ => None,
        });
        assert_eq!(n.cooldown_secs, 120);
        assert_eq!(n.escalate_window_secs, 300);
        assert_eq!(n.escalate_repeat_threshold, 3);

        let n = EscalationNotifier::from_env(|k| match k {
            ENV_NOTIFY_COOLDOWN_SECS => Some("garbage".into()),
            _ => None,
        });
        assert_eq!(n.cooldown_secs, DEFAULT_NOTIFY_COOLDOWN_SECS);
        assert_eq!(n.escalate_window_secs, DEFAULT_ESCALATE_WINDOW_SECS);
        assert_eq!(
            n.escalate_repeat_threshold,
            DEFAULT_ESCALATE_REPEAT_THRESHOLD
        );

        // Zero cooldown clamps to 1 so the anti-spam guarantee always holds.
        let n = EscalationNotifier::new(0, 0, 0);
        assert_eq!(n.cooldown_secs, 1);
        assert_eq!(n.escalate_window_secs, 1);
        assert_eq!(n.escalate_repeat_threshold, 1);
    }

    #[test]
    fn escalation_role_defaults_and_overrides() {
        assert_eq!(
            EscalationNotifier::escalation_role(|_| None),
            DEFAULT_ESCALATION_ROLE
        );
        assert_eq!(
            EscalationNotifier::escalation_role(
                |k| (k == ENV_ESCALATION_ROLE).then(|| "beacon".to_string())
            ),
            "beacon"
        );
        // Blank override falls back to the default.
        assert_eq!(
            EscalationNotifier::escalation_role(
                |k| (k == ENV_ESCALATION_ROLE).then(|| "  ".to_string())
            ),
            DEFAULT_ESCALATION_ROLE
        );
    }
}
