//! Turn-lifecycle timing wall, shared by the hotel's zombie reaper and the
//! guest-side turn watchdog.
//!
//! These two watchdogs are independent processes with no handshake between
//! them, so their budgets have to be layered deliberately. They were not:
//! `heal-dispatcher` reaped at 300s while philote's `evict_timed_out_turns`
//! used a 300s per-phase budget under a 600s aggregate ceiling. The hotel
//! therefore always won, and the guest's own timeout logic could never run.
//!
//! Live evidence (2026-08-05, all three hotels): every failing turn died in a
//! 301–328s band and none survived past it — the distribution was bimodal,
//! either sub-20s or reaped, with nothing in between. 46 of 96 turns across
//! the fleet failed that way, including 18 user-facing Beacon turns on
//! vps-jane that produced no response at all. The guest's 600s ceiling was
//! unreachable dead code, and a turn legitimately waiting on a slow tool was
//! killed as a zombie instead of failing with an honest error.
//!
//! The layering rule: the guest owns turn lifecycle and reports real errors;
//! the hotel reaper is a backstop for turns the guest can no longer speak for
//! (crashed, wedged, or disconnected). The backstop must therefore sit
//! strictly *above* the guest's aggregate ceiling. [`GUEST_TOTAL_CEILING_SECS`]
//! mirrors that ceiling here so the invariant can be enforced at compile time
//! by the assertion below.

/// The hotel fails any turn still `running` this many seconds after its
/// `started_at`. `heal-dispatcher` passes this as
/// `RepairStaleSessionTurns { min_age_secs }` on every sweep.
///
/// There is no heartbeat that resets this clock, so it is a hard wall-clock
/// ceiling on a single turn — not an iteration ceiling. Anything the guest
/// wants to bound itself must finish comfortably inside it.
pub const TURN_ZOMBIE_REAP_SECS: u64 = 660;

/// Mirror of philote's `MAX_TOTAL_ACTIVE_SECS` — the guest-side aggregate
/// budget for a single active turn, across every phase.
///
/// Kept here purely so the layering invariant is checkable from one place.
/// If philote's ceiling changes, change this with it; the compile-time
/// assertion below will fail the build if the two watchdogs ever cross again.
pub const GUEST_TOTAL_CEILING_SECS: u64 = 600;

/// The invariant that was missing. A build in which the hotel's backstop
/// fires at or before the guest's own ceiling is a build where slow-but-
/// healthy turns die as `ZOMBIE_TURN_REPAIR` and the operator gets silence
/// instead of an error.
const _: () = assert!(
    GUEST_TOTAL_CEILING_SECS < TURN_ZOMBIE_REAP_SECS,
    "the hotel's zombie reaper must outlast the guest's own turn ceiling, \
     otherwise the guest can never report its own timeout"
);
