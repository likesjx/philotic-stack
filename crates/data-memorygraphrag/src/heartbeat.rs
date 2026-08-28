//! Philotic heartbeat — hotel-managed deterministic sensors that fire an
//! agent turn ONLY when there is real work.
//!
//! Operator-directed (2026-08-27): sensing runs deterministically first —
//! exact queries against the graph, real state stamps, no model — and a
//! model turn is spent only when a check finds something. The heartbeat is
//! part of the HOTEL, not deployment tooling: it ticks inside the
//! life-graph-runner (like the hygiene sweep), ships with every build, and
//! needs no per-host installation. Because the runner materializes on one
//! hotel, single-deliverer (Beacon, the chief of staff) is structural.
//!
//! v1 check — reminders: due-dated live nodes entering the next tick window
//! (24h grace re-catches stragglers) that lack `reminder_dispatched_at`.
//! The tick stamps them (exact at-most-once dispatch — stamped-then-emit,
//! so a failed emit is caught by the daily-brief safety net rather than
//! risking duplicates) and pre-formats the ⏰ lines, so the agent turn is
//! pure delivery.

use crate::ontology;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;

pub const HEARTBEAT_ENABLED_ENV: &str = "PHILOTIC_HEARTBEAT_ENABLED";
pub const HEARTBEAT_INTERVAL_ENV: &str = "PHILOTIC_HEARTBEAT_INTERVAL_SECS";
pub const HEARTBEAT_CHAT_ID_ENV: &str = "PHILOTIC_HEARTBEAT_CHAT_ID";
pub const HEARTBEAT_TARGET_ROLE_ENV: &str = "PHILOTIC_HEARTBEAT_TARGET_ROLE";
pub const OPERATOR_TZ_ENV: &str = "PHILOTIC_OPERATOR_TZ";

pub const DEFAULT_INTERVAL_SECS: u64 = 300;
pub const MIN_INTERVAL_SECS: u64 = 60;
pub const GRACE_SECONDS: i64 = 24 * 3600;
pub const MAX_REMINDERS_PER_TICK: usize = 10;

/// Labels that carry operator-facing due dates.
pub const REMINDER_LABELS: &[&str] = &[
    "NextAction",
    "Commitment",
    "Appointment",
    "Event",
    "OpenLoop",
];

/// Default ON: reminders are operator-facing core behavior.
pub fn enabled_from_env() -> bool {
    match std::env::var(HEARTBEAT_ENABLED_ENV) {
        Ok(v) => {
            let v = v.trim();
            !(v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("no"))
        }
        Err(_) => true,
    }
}

pub fn interval_secs_from_env() -> u64 {
    std::env::var(HEARTBEAT_INTERVAL_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|v| v.max(MIN_INTERVAL_SECS))
        .unwrap_or(DEFAULT_INTERVAL_SECS)
}

/// Without a chat id the sensor still runs (visibility) but nothing can be
/// delivered — the caller logs and skips dispatch.
pub fn chat_id_from_env() -> Option<String> {
    std::env::var(HEARTBEAT_CHAT_ID_ENV)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn target_role_from_env() -> String {
    std::env::var(HEARTBEAT_TARGET_ROLE_ENV)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "role:agent-beacon:orchestrator".to_string())
}

/// Operator timezone from the operator-time plane (`PHILOTIC_OPERATOR_TZ`),
/// falling back to US Eastern.
pub fn operator_tz() -> Tz {
    std::env::var(OPERATOR_TZ_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<Tz>().ok())
        .unwrap_or(chrono_tz::America::New_York)
}

fn iso(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Due-reminder selection: live nodes with `due_at` inside
/// `[now - grace, now + window]` and no dispatch stamp. Vocabulary comes
/// from the central ontology — the same liveness predicate every other
/// surface uses.
pub fn due_reminders_cypher(now: DateTime<Utc>, window_secs: u64) -> String {
    let horizon = iso(now + chrono::Duration::seconds(window_secs as i64));
    let grace = iso(now - chrono::Duration::seconds(GRACE_SECONDS));
    let labels = REMINDER_LABELS
        .iter()
        .map(|l| format!("'{l}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "MATCH (n) WHERE any(l IN labels(n) WHERE l IN [{labels}]) \
         AND {live} \
         AND n.due_at IS NOT NULL \
         AND n.due_at <= '{horizon}' AND n.due_at >= '{grace}' \
         AND n.reminder_dispatched_at IS NULL \
         RETURN n.id AS id, n.due_at AS due_at, \
         substring(coalesce(n.claim_summary, ''), 0, 160) AS claim \
         ORDER BY n.due_at LIMIT {limit}",
        live = ontology::liveness_predicate("n"),
        limit = MAX_REMINDERS_PER_TICK,
    )
}

/// Stamp selected ids as dispatched. Ids come back from the graph itself but
/// are still single-quote-escaped before interpolation.
pub fn stamp_cypher(ids: &[String], now: DateTime<Utc>) -> String {
    let list = ids
        .iter()
        .map(|id| format!("'{}'", id.replace('\\', "\\\\").replace('\'', "\\'")))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "MATCH (n) WHERE n.id IN [{list}] SET n.reminder_dispatched_at = '{}'",
        iso(now)
    )
}

/// One pre-formatted delivery line; the agent turn sends these verbatim.
pub fn format_reminder_line(claim: &str, due_iso: &str, id: &str, tz: &Tz) -> String {
    let due_local = DateTime::parse_from_rfc3339(&due_iso.replace('Z', "+00:00"))
        .map(|dt| {
            dt.with_timezone(tz)
                .format("%Y-%m-%d %-I:%M %p %Z")
                .to_string()
        })
        .unwrap_or_else(|_| due_iso.to_string());
    format!("⏰ Reminder: {claim} (due {due_local}) — {id}")
}

/// The full message handed to the delivery turn.
pub fn dispatch_message(lines: &[String]) -> String {
    format!(
        "Heartbeat reminder dispatch (deterministic pre-selection — do NOT \
         re-query). Send the operator this reminder text as one Telegram \
         message, verbatim:\n{}",
        lines.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 27, 14, 0, 0).unwrap()
    }

    #[test]
    fn due_query_binds_window_grace_liveness_and_stamp_guard() {
        let c = due_reminders_cypher(now(), 300);
        assert!(c.contains("n.due_at <= '2026-08-27T14:05:00Z'"));
        assert!(c.contains("n.due_at >= '2026-08-26T14:00:00Z'"));
        assert!(c.contains("n.reminder_dispatched_at IS NULL"));
        assert!(c.contains("'resolved'"), "liveness predicate must apply");
        for label in REMINDER_LABELS {
            assert!(c.contains(&format!("'{label}'")));
        }
    }

    #[test]
    fn stamp_escapes_ids() {
        let c = stamp_cypher(&["life:x'y".to_string()], now());
        assert!(c.contains("'life:x\\'y'"));
        assert!(c.contains("n.reminder_dispatched_at = '2026-08-27T14:00:00Z'"));
    }

    #[test]
    fn reminder_line_renders_operator_local_time() {
        let line = format_reminder_line(
            "Call the nephrologist",
            "2026-08-27T18:00:00Z",
            "life:next_action:x",
            &chrono_tz::America::New_York,
        );
        assert!(line.contains("2:00 PM EDT"), "{line}");
        assert!(line.starts_with("⏰ Reminder: Call the nephrologist"));
        assert!(line.ends_with("life:next_action:x"));
    }

    #[test]
    fn env_defaults_are_safe() {
        assert!(interval_secs_from_env() >= MIN_INTERVAL_SECS);
        assert_eq!(target_role_from_env(), "role:agent-beacon:orchestrator");
    }
}
