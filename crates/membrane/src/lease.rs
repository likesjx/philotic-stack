//! Lease lifecycle types shared across all membrane variants.
//!
//! Two layers live here:
//!
//! 1. [`LeaseRenewResult`] / [`LeaseAcquireOutcome`] — the wire-shaped outcomes
//!    every transport already maps its IPC responses onto.
//! 2. [`LeaseDriver`] — a reusable, transport-agnostic lease state machine that
//!    encodes the union of behaviors learned the hard way across the Telegram,
//!    Discord, MCP, and desktop membranes:
//!
//!    * **Epoch fencing** — every renew echoes the last epoch the hotel handed
//!      us; the hotel rejects stale holders.
//!    * **Re-acquire in place** — `granted: false, lease: None` on renew means
//!      "no current owner" (the hotel reset or expired the lease), not "we are
//!      fenced out". The correct move is to re-acquire immediately, not to tear
//!      the seat down (a seat restart escalates error backoff and turns a blip
//!      into a multi-minute outage).
//!    * **Exponential backoff with reset-on-success** — IPC errors back off
//!      1s → 2s → 4s → … → 600s; any successful acquire/renew resets to 1s.
//!    * **Machine-sleep tolerance** — if the gap since the last successful
//!      renewal exceeds the TTL (laptop lid closed, VM paused), the hotel has
//!      already expired the lease server-side. Renewing with the stale epoch is
//!      pointless; the driver goes straight to re-acquire.
//!    * **NetworkState-aware suppression** — while the caller reports offline,
//!      the driver performs no IPC at all; on reconnect it acts immediately
//!      (renew if the TTL cannot have lapsed, otherwise straight re-acquire,
//!      and a `NeedsReacquire` answer still falls through to re-acquire within
//!      the same tick).
//!
//! The driver owns *policy and timing state only*. It never holds an IPC
//! client: every tick borrows a [`LeaseBackend`], so transports can wrap
//! `&mut PhiloticClient` plus their lease-request enum variants in a
//! short-lived adapter struct.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time::Instant;

/// Outcome of a lease renewal attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum LeaseRenewResult {
    /// Renewal succeeded. The new epoch is returned.
    Ok { epoch: u64 },
    /// The lease was lost and needs to be re-acquired from scratch.
    NeedsReacquire,
    /// Another holder took the lease. `owner` is their guest_id.
    Lost { owner: Option<String> },
}

impl LeaseRenewResult {
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }
}

/// Outcome of a lease acquire attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum LeaseAcquireOutcome {
    /// The lease was granted to us at `epoch`.
    Granted { epoch: u64 },
    /// Another holder owns the lease. `owner` is their guest_id if known.
    Held { owner: Option<String>, epoch: u64 },
}

/// Transport-specific IPC adapter the [`LeaseDriver`] drives.
///
/// Implementations translate these calls into their concrete lease-request
/// enum variants (`AcquireTelegramPollLease`, `RenewDiscordGatewayLease`, …)
/// and map the responses back:
///
/// * acquire `granted: true` → [`LeaseAcquireOutcome::Granted`]
/// * acquire `granted: false` → [`LeaseAcquireOutcome::Held`]
/// * renew `granted: true, lease: Some` → [`LeaseRenewResult::Ok`]
/// * renew `granted: false, lease: None` → [`LeaseRenewResult::NeedsReacquire`]
/// * renew `granted: false, lease: Some(other)` → [`LeaseRenewResult::Lost`]
///
/// Transport/IPC failures surface as `Err` and are absorbed into the driver's
/// backoff machinery — they are never fatal by themselves.
///
/// The trait is deliberately not `'static`: implement it on a short-lived
/// struct borrowing `&mut PhiloticClient` and pass it to each
/// [`LeaseDriver::tick`] call.
#[async_trait]
pub trait LeaseBackend: Send {
    /// Acquire (or re-acquire) the lease from scratch.
    async fn acquire(&mut self) -> Result<LeaseAcquireOutcome>;

    /// Renew the lease, echoing `epoch` in the IPC request (epoch fencing).
    async fn renew(&mut self, epoch: u64) -> Result<LeaseRenewResult>;

    /// Release the lease on clean shutdown.
    async fn release(&mut self) -> Result<()>;
}

/// Tunables for [`LeaseDriver`]. Defaults match the deployed hotel policy:
/// TTL 90s, renew every 20s, backoff 1s → 600s.
#[derive(Debug, Clone)]
pub struct LeaseDriverConfig {
    /// Hotel-side lease TTL. A renewal gap larger than this means the hotel
    /// has expired us and the driver re-acquires instead of renewing.
    pub ttl: Duration,
    /// Interval between successful renewals.
    pub renew_interval: Duration,
    /// First retry delay after an IPC error.
    pub backoff_initial: Duration,
    /// Retry delay ceiling.
    pub backoff_max: Duration,
}

impl Default for LeaseDriverConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(90),
            renew_interval: Duration::from_secs(20),
            backoff_initial: Duration::from_secs(1),
            backoff_max: Duration::from_secs(600),
        }
    }
}

/// What a [`LeaseDriver::tick`] did (or decided not to do).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseEvent {
    /// Initial acquire succeeded.
    Acquired { epoch: u64 },
    /// Scheduled renewal succeeded.
    Renewed { epoch: u64 },
    /// Lease was re-acquired in place after a lapse
    /// (`NeedsReacquire`, renew IPC error, sleep gap, or reconnect).
    Reacquired { epoch: u64 },
    /// Another holder owns the lease. Terminal: the seat should yield.
    Lost { owner: Option<String> },
    /// IPC error; the driver will retry after `retry_in`.
    BackingOff { retry_in: Duration, error: String },
    /// Caller reported offline; no IPC attempted.
    Offline,
    /// Nothing due yet (called before `next_deadline`), or already released.
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    /// No lease held; next attempt is an acquire.
    /// `reacquire` distinguishes re-acquire-in-place from the initial grab.
    Acquiring { reacquire: bool },
    /// Lease held at `epoch`; next attempt is a renew (unless the TTL gap
    /// check demotes it to a re-acquire).
    Held { epoch: u64 },
    /// Another holder owns the lease. Terminal.
    Lost { owner: Option<String> },
    /// Released by the caller. Terminal.
    Released,
}

/// Reusable lease state machine. See the module docs for the behavior union.
///
/// Integration contract:
///
/// * Call [`tick`](Self::tick) whenever `now >= next_deadline()`; between
///   deadlines the transport runs its own loop (polling, gateway reads, …).
///   Ticking early is harmless (returns [`LeaseEvent::Idle`]).
/// * Feed connectivity transitions through [`set_online`](Self::set_online).
/// * Treat [`LeaseEvent::Lost`] as "another seat owns this resource — yield".
///   Everything else is survivable and already scheduled for retry.
#[derive(Debug)]
pub struct LeaseDriver {
    config: LeaseDriverConfig,
    phase: Phase,
    /// Instant of the last successful acquire/renew (for the TTL gap check).
    last_success: Option<Instant>,
    /// Earliest instant the next IPC attempt may run.
    next_attempt: Instant,
    /// Delay to apply on the *next* IPC error.
    backoff: Duration,
    online: bool,
}

impl LeaseDriver {
    pub fn new(config: LeaseDriverConfig) -> Self {
        let backoff = config.backoff_initial;
        Self {
            config,
            phase: Phase::Acquiring { reacquire: false },
            last_success: None,
            next_attempt: Instant::now(),
            backoff,
            online: true,
        }
    }

    /// Current epoch, if the lease is held.
    pub fn epoch(&self) -> Option<u64> {
        match self.phase {
            Phase::Held { epoch } => Some(epoch),
            _ => None,
        }
    }

    pub fn is_held(&self) -> bool {
        matches!(self.phase, Phase::Held { .. })
    }

    /// True once the lease has been lost to another holder (terminal).
    pub fn is_lost(&self) -> bool {
        matches!(self.phase, Phase::Lost { .. })
    }

    /// When the caller should next invoke [`tick`](Self::tick).
    /// `None` while offline or in a terminal state (lost / released).
    pub fn next_deadline(&self) -> Option<Instant> {
        if !self.online {
            return None;
        }
        match self.phase {
            Phase::Lost { .. } | Phase::Released => None,
            _ => Some(self.next_attempt),
        }
    }

    /// Feed a NetworkState transition. While offline the driver performs no
    /// IPC. On the offline→online edge the backoff resets and the next tick
    /// runs immediately: a renew if the TTL cannot have lapsed, otherwise a
    /// straight re-acquire (and a renew answered with `NeedsReacquire` still
    /// falls through to re-acquire within the same tick).
    pub fn set_online(&mut self, online: bool) {
        if online == self.online {
            return;
        }
        self.online = online;
        if online {
            self.backoff = self.config.backoff_initial;
            self.next_attempt = Instant::now();
        }
    }

    /// Run one step of the state machine if an attempt is due.
    ///
    /// Never fails: backend `Err`s are absorbed as [`LeaseEvent::BackingOff`].
    pub async fn tick<B: LeaseBackend + ?Sized>(&mut self, backend: &mut B) -> LeaseEvent {
        match &self.phase {
            Phase::Lost { owner } => {
                return LeaseEvent::Lost {
                    owner: owner.clone(),
                };
            }
            Phase::Released => return LeaseEvent::Idle,
            _ => {}
        }
        if !self.online {
            return LeaseEvent::Offline;
        }
        let now = Instant::now();
        if now < self.next_attempt {
            return LeaseEvent::Idle;
        }

        match self.phase.clone() {
            Phase::Acquiring { reacquire } => self.do_acquire(backend, now, reacquire).await,
            Phase::Held { epoch } => {
                // Machine-sleep / long-offline tolerance: if we have not
                // renewed within the TTL, the hotel has already expired the
                // lease. Skip the doomed stale-epoch renew and re-acquire.
                let lapsed = self
                    .last_success
                    .map(|t| now.duration_since(t) > self.config.ttl)
                    .unwrap_or(true);
                if lapsed {
                    return self.do_acquire(backend, now, true).await;
                }
                match backend.renew(epoch).await {
                    Ok(LeaseRenewResult::Ok { epoch }) => {
                        self.on_success(epoch, now);
                        LeaseEvent::Renewed { epoch }
                    }
                    Ok(LeaseRenewResult::NeedsReacquire) => {
                        // No current owner — re-acquire in place, same tick.
                        self.do_acquire(backend, now, true).await
                    }
                    Ok(LeaseRenewResult::Lost { owner }) => {
                        self.phase = Phase::Lost {
                            owner: owner.clone(),
                        };
                        LeaseEvent::Lost { owner }
                    }
                    Err(_renew_err) => {
                        // A renew can fail because the hotel let the lease
                        // lapse while the seat was quiet — nobody else holds
                        // it, so try an immediate re-acquire in place before
                        // falling back to backoff.
                        self.do_acquire(backend, now, true).await
                    }
                }
            }
            Phase::Lost { .. } | Phase::Released => unreachable!("handled above"),
        }
    }

    /// Release the lease (clean shutdown). Terminal on success.
    pub async fn release<B: LeaseBackend + ?Sized>(&mut self, backend: &mut B) -> Result<()> {
        if matches!(self.phase, Phase::Released) {
            return Ok(());
        }
        backend.release().await?;
        self.phase = Phase::Released;
        Ok(())
    }

    async fn do_acquire<B: LeaseBackend + ?Sized>(
        &mut self,
        backend: &mut B,
        now: Instant,
        reacquire: bool,
    ) -> LeaseEvent {
        match backend.acquire().await {
            Ok(LeaseAcquireOutcome::Granted { epoch }) => {
                self.on_success(epoch, now);
                if reacquire {
                    LeaseEvent::Reacquired { epoch }
                } else {
                    LeaseEvent::Acquired { epoch }
                }
            }
            Ok(LeaseAcquireOutcome::Held { owner, .. }) => {
                self.phase = Phase::Lost {
                    owner: owner.clone(),
                };
                LeaseEvent::Lost { owner }
            }
            Err(err) => {
                self.phase = Phase::Acquiring { reacquire };
                let retry_in = self.backoff;
                self.next_attempt = now + retry_in;
                self.backoff = next_backoff(self.backoff, &self.config);
                LeaseEvent::BackingOff {
                    retry_in,
                    error: format!("{err:#}"),
                }
            }
        }
    }

    fn on_success(&mut self, epoch: u64, now: Instant) {
        self.phase = Phase::Held { epoch };
        self.last_success = Some(now);
        self.next_attempt = now + self.config.renew_interval;
        self.backoff = self.config.backoff_initial;
    }
}

/// Exponential backoff step: doubles, clamped to `[initial, max]`.
fn next_backoff(current: Duration, config: &LeaseDriverConfig) -> Duration {
    current
        .saturating_mul(2)
        .clamp(config.backoff_initial, config.backoff_max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use std::collections::VecDeque;
    use tokio::time::advance;

    /// Scripted mock hotel: pre-loaded acquire/renew answers plus a call log.
    #[derive(Default)]
    struct ScriptedBackend {
        acquire_results: VecDeque<Result<LeaseAcquireOutcome>>,
        renew_results: VecDeque<Result<LeaseRenewResult>>,
        /// Every call in order: `Acquire`, `Renew(epoch echoed)`, `Release`.
        calls: Vec<Call>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Acquire,
        Renew(u64),
        Release,
    }

    #[async_trait]
    impl LeaseBackend for ScriptedBackend {
        async fn acquire(&mut self) -> Result<LeaseAcquireOutcome> {
            self.calls.push(Call::Acquire);
            self.acquire_results
                .pop_front()
                .expect("unexpected acquire call — script exhausted")
        }

        async fn renew(&mut self, epoch: u64) -> Result<LeaseRenewResult> {
            self.calls.push(Call::Renew(epoch));
            self.renew_results
                .pop_front()
                .expect("unexpected renew call — script exhausted")
        }

        async fn release(&mut self) -> Result<()> {
            self.calls.push(Call::Release);
            Ok(())
        }
    }

    fn driver() -> LeaseDriver {
        LeaseDriver::new(LeaseDriverConfig::default())
    }

    #[tokio::test(start_paused = true)]
    async fn grant_then_renew_ok_with_epoch_echo() {
        let mut hotel = ScriptedBackend::default();
        hotel
            .acquire_results
            .push_back(Ok(LeaseAcquireOutcome::Granted { epoch: 1 }));
        hotel
            .renew_results
            .push_back(Ok(LeaseRenewResult::Ok { epoch: 2 }));
        hotel
            .renew_results
            .push_back(Ok(LeaseRenewResult::Ok { epoch: 3 }));

        let mut d = driver();
        assert_eq!(d.tick(&mut hotel).await, LeaseEvent::Acquired { epoch: 1 });
        assert_eq!(d.epoch(), Some(1));

        // Not due yet — no IPC.
        assert_eq!(d.tick(&mut hotel).await, LeaseEvent::Idle);

        advance(Duration::from_secs(20)).await;
        assert_eq!(d.tick(&mut hotel).await, LeaseEvent::Renewed { epoch: 2 });

        advance(Duration::from_secs(20)).await;
        assert_eq!(d.tick(&mut hotel).await, LeaseEvent::Renewed { epoch: 3 });

        // Epoch fencing: each renew echoed the epoch granted previously.
        assert_eq!(
            hotel.calls,
            vec![Call::Acquire, Call::Renew(1), Call::Renew(2)]
        );
        assert_eq!(d.epoch(), Some(3));
    }

    #[tokio::test(start_paused = true)]
    async fn renew_no_owner_reacquires_in_place_same_tick() {
        let mut hotel = ScriptedBackend::default();
        hotel
            .acquire_results
            .push_back(Ok(LeaseAcquireOutcome::Granted { epoch: 1 }));
        // Hotel reset the lease table: renew says "no owner".
        hotel
            .renew_results
            .push_back(Ok(LeaseRenewResult::NeedsReacquire));
        hotel
            .acquire_results
            .push_back(Ok(LeaseAcquireOutcome::Granted { epoch: 7 }));
        hotel
            .renew_results
            .push_back(Ok(LeaseRenewResult::Ok { epoch: 8 }));

        let mut d = driver();
        assert_eq!(d.tick(&mut hotel).await, LeaseEvent::Acquired { epoch: 1 });

        advance(Duration::from_secs(20)).await;
        // One tick: renew → NeedsReacquire → acquire, no seat teardown.
        assert_eq!(
            d.tick(&mut hotel).await,
            LeaseEvent::Reacquired { epoch: 7 }
        );
        assert!(d.is_held());

        advance(Duration::from_secs(20)).await;
        assert_eq!(d.tick(&mut hotel).await, LeaseEvent::Renewed { epoch: 8 });

        // Epoch echo tracks the re-acquired epoch, not the stale one.
        assert_eq!(
            hotel.calls,
            vec![Call::Acquire, Call::Renew(1), Call::Acquire, Call::Renew(7)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn ipc_errors_grow_backoff_then_reset_on_success() {
        let mut hotel = ScriptedBackend::default();
        hotel
            .acquire_results
            .push_back(Ok(LeaseAcquireOutcome::Granted { epoch: 1 }));
        // Renew errors → driver tries immediate re-acquire, which also errors.
        hotel.renew_results.push_back(Err(anyhow!("ipc down")));
        hotel.acquire_results.push_back(Err(anyhow!("ipc down")));
        // Two more acquire attempts fail while backing off.
        hotel.acquire_results.push_back(Err(anyhow!("ipc down")));
        hotel.acquire_results.push_back(Err(anyhow!("ipc down")));
        // Fourth attempt succeeds.
        hotel
            .acquire_results
            .push_back(Ok(LeaseAcquireOutcome::Granted { epoch: 5 }));
        // Then another failure to prove the backoff reset to 1s.
        hotel.renew_results.push_back(Err(anyhow!("ipc down")));
        hotel.acquire_results.push_back(Err(anyhow!("ipc down")));

        let mut d = driver();
        assert_eq!(d.tick(&mut hotel).await, LeaseEvent::Acquired { epoch: 1 });

        advance(Duration::from_secs(20)).await;
        let e1 = d.tick(&mut hotel).await;
        assert!(
            matches!(e1, LeaseEvent::BackingOff { retry_in, .. } if retry_in == Duration::from_secs(1)),
            "expected 1s backoff, got {e1:?}"
        );

        advance(Duration::from_secs(1)).await;
        let e2 = d.tick(&mut hotel).await;
        assert!(
            matches!(e2, LeaseEvent::BackingOff { retry_in, .. } if retry_in == Duration::from_secs(2)),
            "expected 2s backoff, got {e2:?}"
        );

        advance(Duration::from_secs(2)).await;
        let e3 = d.tick(&mut hotel).await;
        assert!(
            matches!(e3, LeaseEvent::BackingOff { retry_in, .. } if retry_in == Duration::from_secs(4)),
            "expected 4s backoff, got {e3:?}"
        );

        advance(Duration::from_secs(4)).await;
        assert_eq!(
            d.tick(&mut hotel).await,
            LeaseEvent::Reacquired { epoch: 5 }
        );

        // Success reset the backoff: the next error starts at 1s again.
        advance(Duration::from_secs(20)).await;
        let e4 = d.tick(&mut hotel).await;
        assert!(
            matches!(e4, LeaseEvent::BackingOff { retry_in, .. } if retry_in == Duration::from_secs(1)),
            "expected backoff reset to 1s, got {e4:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn backoff_is_clamped_to_max() {
        let config = LeaseDriverConfig::default();
        let mut b = config.backoff_initial;
        for _ in 0..32 {
            b = next_backoff(b, &config);
        }
        assert_eq!(b, Duration::from_secs(600));
    }

    #[tokio::test(start_paused = true)]
    async fn offline_suppresses_then_reconnect_reacquires_immediately() {
        let mut hotel = ScriptedBackend::default();
        hotel
            .acquire_results
            .push_back(Ok(LeaseAcquireOutcome::Granted { epoch: 1 }));
        hotel
            .acquire_results
            .push_back(Ok(LeaseAcquireOutcome::Granted { epoch: 2 }));

        let mut d = driver();
        assert_eq!(d.tick(&mut hotel).await, LeaseEvent::Acquired { epoch: 1 });

        d.set_online(false);
        assert_eq!(d.next_deadline(), None);

        // Offline across several renew intervals and past the TTL: driver
        // stays silent — no renew spam against a dead network.
        for _ in 0..10 {
            advance(Duration::from_secs(20)).await;
            assert_eq!(d.tick(&mut hotel).await, LeaseEvent::Offline);
        }
        assert_eq!(hotel.calls, vec![Call::Acquire]);

        // Reconnect: gap (200s) > TTL (90s) ⇒ immediate re-acquire, and no
        // doomed stale-epoch renew is attempted first.
        d.set_online(true);
        assert_eq!(
            d.tick(&mut hotel).await,
            LeaseEvent::Reacquired { epoch: 2 }
        );
        assert_eq!(hotel.calls, vec![Call::Acquire, Call::Acquire]);
        assert_eq!(d.epoch(), Some(2));
    }

    #[tokio::test(start_paused = true)]
    async fn short_offline_blip_renews_on_reconnect() {
        let mut hotel = ScriptedBackend::default();
        hotel
            .acquire_results
            .push_back(Ok(LeaseAcquireOutcome::Granted { epoch: 1 }));
        hotel
            .renew_results
            .push_back(Ok(LeaseRenewResult::Ok { epoch: 2 }));

        let mut d = driver();
        assert_eq!(d.tick(&mut hotel).await, LeaseEvent::Acquired { epoch: 1 });

        // Blip shorter than the TTL: lease is still valid hotel-side.
        d.set_online(false);
        advance(Duration::from_secs(40)).await;
        d.set_online(true);

        assert_eq!(d.tick(&mut hotel).await, LeaseEvent::Renewed { epoch: 2 });
        assert_eq!(hotel.calls, vec![Call::Acquire, Call::Renew(1)]);
    }

    #[tokio::test(start_paused = true)]
    async fn sleep_gap_past_ttl_reacquires_instead_of_renewing() {
        let mut hotel = ScriptedBackend::default();
        hotel
            .acquire_results
            .push_back(Ok(LeaseAcquireOutcome::Granted { epoch: 1 }));
        hotel
            .acquire_results
            .push_back(Ok(LeaseAcquireOutcome::Granted { epoch: 9 }));

        let mut d = driver();
        assert_eq!(d.tick(&mut hotel).await, LeaseEvent::Acquired { epoch: 1 });

        // Machine sleep: no offline signal, the clock just blows past the TTL.
        advance(Duration::from_secs(120)).await;
        assert_eq!(
            d.tick(&mut hotel).await,
            LeaseEvent::Reacquired { epoch: 9 }
        );

        // No renew with the stale epoch was ever attempted.
        assert_eq!(hotel.calls, vec![Call::Acquire, Call::Acquire]);
        assert_eq!(d.epoch(), Some(9));
    }

    #[tokio::test(start_paused = true)]
    async fn renew_ipc_error_triggers_immediate_reacquire_in_place() {
        let mut hotel = ScriptedBackend::default();
        hotel
            .acquire_results
            .push_back(Ok(LeaseAcquireOutcome::Granted { epoch: 1 }));
        hotel.renew_results.push_back(Err(anyhow!("renew lapsed")));
        hotel
            .acquire_results
            .push_back(Ok(LeaseAcquireOutcome::Granted { epoch: 4 }));

        let mut d = driver();
        assert_eq!(d.tick(&mut hotel).await, LeaseEvent::Acquired { epoch: 1 });

        advance(Duration::from_secs(20)).await;
        // Renew errored, but the seat does NOT die and does NOT wait out a
        // backoff: it re-acquires in the same tick.
        assert_eq!(
            d.tick(&mut hotel).await,
            LeaseEvent::Reacquired { epoch: 4 }
        );
        assert_eq!(
            hotel.calls,
            vec![Call::Acquire, Call::Renew(1), Call::Acquire]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn lost_to_other_holder_is_terminal() {
        let mut hotel = ScriptedBackend::default();
        hotel
            .acquire_results
            .push_back(Ok(LeaseAcquireOutcome::Granted { epoch: 1 }));
        hotel.renew_results.push_back(Ok(LeaseRenewResult::Lost {
            owner: Some("other-seat".into()),
        }));

        let mut d = driver();
        assert_eq!(d.tick(&mut hotel).await, LeaseEvent::Acquired { epoch: 1 });

        advance(Duration::from_secs(20)).await;
        assert_eq!(
            d.tick(&mut hotel).await,
            LeaseEvent::Lost {
                owner: Some("other-seat".into())
            }
        );
        assert!(d.is_lost());
        assert_eq!(d.next_deadline(), None);

        // Further ticks repeat the terminal event without touching the hotel.
        advance(Duration::from_secs(20)).await;
        assert_eq!(
            d.tick(&mut hotel).await,
            LeaseEvent::Lost {
                owner: Some("other-seat".into())
            }
        );
        assert_eq!(hotel.calls, vec![Call::Acquire, Call::Renew(1)]);
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_denied_by_other_holder_reports_lost() {
        let mut hotel = ScriptedBackend::default();
        hotel
            .acquire_results
            .push_back(Ok(LeaseAcquireOutcome::Held {
                owner: Some("incumbent".into()),
                epoch: 12,
            }));

        let mut d = driver();
        assert_eq!(
            d.tick(&mut hotel).await,
            LeaseEvent::Lost {
                owner: Some("incumbent".into())
            }
        );
        assert!(d.is_lost());
    }

    #[tokio::test(start_paused = true)]
    async fn release_is_terminal_and_idempotent() {
        let mut hotel = ScriptedBackend::default();
        hotel
            .acquire_results
            .push_back(Ok(LeaseAcquireOutcome::Granted { epoch: 1 }));

        let mut d = driver();
        assert_eq!(d.tick(&mut hotel).await, LeaseEvent::Acquired { epoch: 1 });

        d.release(&mut hotel).await.unwrap();
        assert_eq!(d.next_deadline(), None);
        assert!(!d.is_held());

        // No further IPC after release, and release is idempotent.
        advance(Duration::from_secs(60)).await;
        assert_eq!(d.tick(&mut hotel).await, LeaseEvent::Idle);
        d.release(&mut hotel).await.unwrap();
        assert_eq!(hotel.calls, vec![Call::Acquire, Call::Release]);
    }
}
