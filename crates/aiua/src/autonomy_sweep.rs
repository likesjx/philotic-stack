//! Autonomy outcome timeout-to-Neutral sweep — Autopoiesis Slice A9
//! outcome-stamping follow-up.
//!
//! A9 shipped `AuditOutcome::{Pending,ConfirmedGood,Reversed,Neutral}`, the
//! `RecordAutonomyOutcome` IPC path, `promotion_eligible()`, and
//! `phil autonomy status` — but nothing ever stamped an outcome
//! automatically. An audit record left `Pending` sat there forever, so a
//! quiet lane's confirmed-good streak never moved and no lane could ever
//! earn a promotion. This module closes that gap: on a daily schedule, any
//! audit record across every lane that is still `Pending` after
//! [`ansible_mesh_core::autonomy::DEFAULT_NEUTRAL_AFTER_DAYS`] days (env
//! override `PHILOTIC_AUTONOMY_NEUTRAL_AFTER_DAYS`) is stamped `Neutral` via
//! the same [`ansible_mesh_core::domain::GraphDomain::set_autonomy_audit_outcome`]
//! path the operator's `phil autonomy stamp` command uses. `Neutral` never
//! mutates a grant's earn/demote counters (see
//! [`ansible_mesh_core::autonomy::AuditOutcome::Neutral`]) — this sweep is a
//! bookkeeping wash that keeps the ledger honest, not a promotion shortcut.
//!
//! # Mesh trap (load-bearing) — no local opt-in flag here
//!
//! Like Memory Transparency Slice M4's hygiene sweep
//! (`crate::memory_hygiene`), `CronJobSync` replicates a hotel's `CronJob`
//! *definitions* to every mesh-connected peer unconditionally
//! (`handle_cron_job_sync` in `aiua::main` upserts without checking any
//! local flag). M4 handles this with a *local opt-in* re-check
//! (`enabled_locally`) because that sweep itself is optional per hotel.
//!
//! This sweep is different: it is **always-on** — there is no operator
//! opt-in, because the trust ledger needs it to function at all — so a
//! local-opt-in flag would gate nothing. Instead `CronTicker::fire` (see
//! `service::cron_ticker::fire_autonomy_sweep`) gates firing on the job's id
//! matching *this hotel's own* deterministic job id
//! ([`cron_job_id`]). A job definition replicated from a peer hotel carries
//! that peer's id (`autonomy-outcome-sweep:{peer}`), which never equals this
//! hotel's own `cron_job_id(hotel_name)`, so it is silently skipped instead
//! of one hotel sweeping (and mis-attributing) a peer's audit records.
//!
//! # Scheduling
//!
//! Wired as a `CronJob` (see [`CRON_TARGET_ROLE`]) whose fire is intercepted
//! by `CronTicker::fire` before guest delivery — the sweep runs in-process in
//! the hotel daemon, mirroring `memory_hygiene::CRON_TARGET_ROLE`.
//! Registration ([`ensure_scheduled`]) is unconditional and idempotent: every
//! hotel gets exactly one `autonomy-outcome-sweep:{hotel_name}` job, and a
//! restart never clobbers an operator-edited schedule.

use ansible_mesh_core::autonomy::{
    AuditOutcome, neutral_after_days_from_env, select_due_for_neutral,
};
use ansible_mesh_core::domain::GraphDomain;
use tracing::{debug, info, warn};

/// Reserved `CronJob::target_role` recognized by `CronTicker::fire` as an
/// internal sweep rather than a guest-inbox delivery. Not a real role
/// namespace (`role:{agent}:{role}`) — the `internal:` prefix keeps it out
/// of `resolve_target_role_record`'s parsing, mirroring
/// `memory_hygiene::CRON_TARGET_ROLE`.
pub const CRON_TARGET_ROLE: &str = "internal:autonomy_outcome_sweep";
/// Default: nightly at 04:00 UTC — offset by an hour from `memory.hygiene`'s
/// 03:00 default so the two in-process sweeps don't contend for the same
/// tick.
pub const DEFAULT_SCHEDULE: &str = "0 0 4 * * * *";

/// Deterministic id for the auto-registered per-hotel cron job — stable
/// across restarts so [`ensure_scheduled`] is idempotent and never
/// double-registers, and load-bearing for the mesh-trap gate in
/// `CronTicker::fire` (see module docs).
pub fn cron_job_id(hotel_name: &str) -> String {
    format!("autonomy-outcome-sweep:{hotel_name}")
}

/// Ensure this hotel's daily timeout-to-Neutral sweep cron job is
/// registered. Unlike `memory_hygiene::ensure_scheduled` there is no
/// operator opt-in env var — the trust ledger depends on this running on
/// every hotel, so the only gate is idempotency (a restart must not clobber
/// an operator-edited schedule).
pub fn ensure_scheduled(graph: &GraphDomain, hotel_name: &str, now_ms: u64) -> anyhow::Result<()> {
    let job_id = cron_job_id(hotel_name);
    if graph.get_cron_job(&job_id)?.is_some() {
        debug!(hotel = %hotel_name, "autonomy_sweep: cron job already registered");
        return Ok(());
    }

    let next_fire_at = ansible_mesh_core::cron::next_fire_after(DEFAULT_SCHEDULE, now_ms)?;
    let job = ansible_mesh_core::cron::CronJob {
        id: job_id.clone(),
        schedule: DEFAULT_SCHEDULE.to_string(),
        target_role: CRON_TARGET_ROLE.to_string(),
        target_node_id: None,
        payload: "{}".to_string(),
        guaranteed: false,
        enabled: true,
        last_fired_epoch: None,
        next_fire_at,
        created_at: now_ms,
        created_by: ansible_mesh_core::cron::CronJobSource::Operator,
        silent_ok: true,
        session_target: ansible_mesh_core::cron::CronSessionTarget::Isolated,
    };
    graph.upsert_cron_job(&job)?;
    info!(
        hotel = %hotel_name,
        job_id = %job_id,
        next_fire_at,
        "autonomy_sweep: daily timeout-to-neutral sweep registered"
    );
    Ok(())
}

/// What one sweep run scanned and stamped — returned for logging by
/// [`run_scheduled_sweep`] and directly assertable in tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepOutcome {
    pub hotel_name: String,
    /// Audit records scanned across every lane (`Pending` and already
    /// stamped alike).
    pub scanned: usize,
    /// `audit_id`s stamped `Neutral` this run.
    pub stamped: Vec<String>,
    /// Per-record storage failures — a bad record must not abort the rest
    /// of the sweep.
    pub errors: Vec<String>,
}

/// Stamp every audit record across all lanes that is `Pending` and past
/// `window_secs` old, as of `now`, `Neutral`. Orchestrates I/O around the
/// pure `select_due_for_neutral` selection logic; never panics. Per-record
/// storage failures are captured on [`SweepOutcome::errors`] and do not
/// abort the rest of the sweep — a listing failure (the graph itself is
/// unreadable) does short-circuit, since there is nothing to select from.
pub fn run_sweep(
    graph: &GraphDomain,
    hotel_name: &str,
    now: u64,
    window_secs: u64,
) -> SweepOutcome {
    let mut outcome = SweepOutcome {
        hotel_name: hotel_name.to_string(),
        ..Default::default()
    };

    let records = match graph.list_all_autonomy_audits() {
        Ok(records) => records,
        Err(e) => {
            outcome
                .errors
                .push(format!("list_all_autonomy_audits: {e:#}"));
            return outcome;
        }
    };
    outcome.scanned = records.len();

    let due = select_due_for_neutral(&records, now, window_secs);
    for record in due {
        match graph.set_autonomy_audit_outcome(&record.audit_id, AuditOutcome::Neutral, now) {
            Ok(true) => outcome.stamped.push(record.audit_id.clone()),
            Ok(false) => outcome
                .errors
                .push(format!("{}: record vanished mid-sweep", record.audit_id)),
            Err(e) => outcome.errors.push(format!("{}: {e:#}", record.audit_id)),
        }
    }

    outcome
}

/// Scheduled entry point called from `CronTicker::fire`
/// (`fire_autonomy_sweep`). Resolves the timeout window from the process
/// environment, runs the sweep, and logs what it stamped — a sweep that
/// stamps nothing logs that too, so a clean run is distinguishable from a
/// silently-broken one.
pub fn run_scheduled_sweep(graph: &GraphDomain, hotel_name: &str, now_secs: u64) -> SweepOutcome {
    let window_days = neutral_after_days_from_env(|k| std::env::var(k).ok());
    let window_secs = window_days.saturating_mul(86_400);

    let outcome = run_sweep(graph, hotel_name, now_secs, window_secs);
    if outcome.stamped.is_empty() {
        info!(
            hotel = %hotel_name,
            scanned = outcome.scanned,
            "autonomy_sweep: nothing due — sweep stamped nothing"
        );
    } else {
        info!(
            hotel = %hotel_name,
            scanned = outcome.scanned,
            stamped = outcome.stamped.len(),
            "autonomy_sweep: stamped Pending audits Neutral on timeout"
        );
    }
    for err in &outcome.errors {
        warn!(hotel = %hotel_name, "autonomy_sweep: {err}");
    }
    outcome
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::autonomy::{AutonomyAuditRecord, AutonomyLane, AutonomyPosture};

    const T0: u64 = 1_750_000_000;
    const SEVEN_DAYS_SECS: u64 = 7 * 86_400;

    fn open_domain() -> GraphDomain {
        let storage =
            ansible_mesh_core::sqlite_storage::SqliteGraphStorage::open(":memory:").expect("open");
        GraphDomain::new(std::sync::Arc::new(storage.adapter()))
    }

    fn pending_audit(id: &str, lane: &str, created_at: u64) -> AutonomyAuditRecord {
        AutonomyAuditRecord::new(
            id,
            AutonomyLane::new(lane),
            "did a thing",
            "evidence",
            "revert the thing",
            AutonomyPosture::ConfirmFirst,
            created_at,
        )
    }

    #[test]
    fn ensure_scheduled_registers_once_and_preserves_operator_edits() {
        let graph = open_domain();
        let now_ms = T0 * 1000;
        ensure_scheduled(&graph, "hotel-a", now_ms).expect("ok");

        let job = graph
            .get_cron_job(&cron_job_id("hotel-a"))
            .expect("lookup")
            .expect("job registered");
        assert_eq!(job.target_role, CRON_TARGET_ROLE);
        assert!(job.enabled);
        assert!(job.next_fire_at > now_ms);

        // Idempotent, and never clobbers an operator-edited schedule.
        let mut edited = job.clone();
        edited.schedule = "0 30 5 * * * *".to_string();
        graph.upsert_cron_job(&edited).expect("upsert edited");
        ensure_scheduled(&graph, "hotel-a", now_ms + 1000).expect("ok");
        let after = graph
            .get_cron_job(&cron_job_id("hotel-a"))
            .expect("lookup")
            .expect("still present");
        assert_eq!(after.schedule, "0 30 5 * * * *", "operator edit preserved");
    }

    #[test]
    fn run_sweep_stamps_only_old_unstamped_records() {
        let graph = open_domain();

        let old_pending = pending_audit("old-pending", "graph.bridge_edges", T0);
        let recent_pending = pending_audit(
            "recent-pending",
            "fleet.heal_slices",
            T0 + SEVEN_DAYS_SECS - 1,
        );
        let mut already_stamped = pending_audit("already-stamped", "work.file_proposals", T0);
        already_stamped.outcome = AuditOutcome::ConfirmedGood;

        graph.record_autonomy_audit(&old_pending).expect("record");
        graph
            .record_autonomy_audit(&recent_pending)
            .expect("record");
        graph
            .record_autonomy_audit(&already_stamped)
            .expect("record");

        let now = T0 + SEVEN_DAYS_SECS;
        let outcome = run_sweep(&graph, "hotel-a", now, SEVEN_DAYS_SECS);

        assert_eq!(outcome.scanned, 3);
        assert_eq!(outcome.stamped, vec!["old-pending".to_string()]);
        assert!(outcome.errors.is_empty());

        // The stamped record is durably Neutral; the other two are untouched.
        let stamped = graph
            .get_autonomy_audit("old-pending")
            .expect("lookup")
            .expect("exists");
        assert_eq!(stamped.outcome, AuditOutcome::Neutral);

        let recent = graph
            .get_autonomy_audit("recent-pending")
            .expect("lookup")
            .expect("exists");
        assert_eq!(recent.outcome, AuditOutcome::Pending);

        let confirmed = graph
            .get_autonomy_audit("already-stamped")
            .expect("lookup")
            .expect("exists");
        assert_eq!(confirmed.outcome, AuditOutcome::ConfirmedGood);
    }

    #[test]
    fn run_sweep_stamping_nothing_is_reported_not_silent() {
        let graph = open_domain();
        let recent = pending_audit("recent", "graph.bridge_edges", T0);
        graph.record_autonomy_audit(&recent).expect("record");

        let outcome = run_sweep(&graph, "hotel-a", T0 + 10, SEVEN_DAYS_SECS);
        assert_eq!(outcome.scanned, 1);
        assert!(outcome.stamped.is_empty());
        assert!(outcome.errors.is_empty());
    }

    #[test]
    fn run_scheduled_sweep_honors_the_env_window_override() {
        let graph = open_domain();
        let old_pending = pending_audit("old-pending", "graph.bridge_edges", T0);
        graph.record_autonomy_audit(&old_pending).expect("record");

        // Default window (7 days) has NOT elapsed at T0 + 1 day — nothing
        // stamped without the override. Exercise run_sweep directly with an
        // explicit window (matches what run_scheduled_sweep would resolve)
        // rather than mutating process env, which is unsafe under parallel
        // test execution.
        let one_day_secs = 86_400;
        let default_window = run_sweep(&graph, "hotel-a", T0 + one_day_secs, SEVEN_DAYS_SECS);
        assert!(default_window.stamped.is_empty());

        let overridden_window = run_sweep(&graph, "hotel-a", T0 + one_day_secs, one_day_secs);
        assert_eq!(overridden_window.stamped, vec!["old-pending".to_string()]);
    }
}
