//! Hotel cron scheduler — fires pre-packaged envelopes on a schedule.
//!
//! ## Slice 1 (local, non-guaranteed)
//! Wakes every second, queries due jobs, materialises a `TaskInvoke` envelope,
//! delivers to the role inbox, appends to the ledger, advances `next_fire_at`.
//!
//! ## Slice 2 (guaranteed, mesh-coordinated)
//! - `guaranteed = true` jobs use a staggered per-hotel offset
//!   (`offset_ms = PHILOTIC_CRON_OFFSET_SECS * 1000`).
//! - Before firing: checks `last_fired_epoch == Some(next_fire_at)` — if true,
//!   another hotel already handled this epoch; skip.
//! - After firing: appends a `CronFired` envelope (broadcast) so peer hotels can
//!   suppress their offset-delayed fire for the same epoch.
//! - Recovery scan at startup: guaranteed jobs where `next_fire_at < now` and
//!   `last_fired_epoch != Some(next_fire_at)` are fired immediately.
//!
//! ## Slice 3 (CronJobSync — mesh job propagation)
//! At startup, all local cron jobs are broadcast as `CronJobSync` upsert events
//! so that peer hotels replicate the definitions and can participate in
//! guaranteed firing without requiring a shared config file.
//!
//! ## Slice 4 (template interpolation)
//! The `payload` field supports `{timestamp}`, `{iso_timestamp}`, `{job_id}`,
//! `{node_id}`, and `{target_role}` placeholders, replaced at fire time via
//! [`ansible_mesh_core::cron::interpolate_payload`].
//!
//! ## Delivery ownership (single consumer)
//! `fire()` is the *sole* delivery owner of a fired job's `TaskInvoke`: it claims
//! the event id in the hotel-wide [`DeliveryClaimRegistry`] before appending to
//! the ledger or delivering/parking. The mesh/ledger consumer
//! (`IpcServer::deliver_event_envelope_or_park`) consults the same claim set, so
//! a second observation of the same envelope can never double-deliver.

use crate::LedgerCommand;
use crate::service::guest_manager::GuestMaterializationRequester;
use crate::service::ipc::{DeliveryClaimRegistry, InboxRegistry, ParkedInboundRegistry};
use ansible_mesh_core::cron::{
    CronInterpolationVars, CronJob, interpolate_payload, next_fire_after,
};
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::event::{EventEnvelope, EventKind, EventPayload};
use ansible_mesh_core::graph::RoleIncarnationRecord;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

pub struct CronTicker {
    graph: Arc<GraphDomain>,
    dispatcher_tx: mpsc::Sender<LedgerCommand>,
    inboxes: InboxRegistry,
    local_node_id: String,
    /// Per-hotel stagger offset for guaranteed jobs (ms).
    /// Derived from `PHILOTIC_CRON_OFFSET_SECS * 1000`. Default 0.
    offset_ms: u64,
    parked_inbound: ParkedInboundRegistry,
    materialization_requester: Option<Arc<dyn GuestMaterializationRequester>>,
    /// Hotel-wide single-delivery claim set shared with the mesh/ledger consumer
    /// (`deliver_event_envelope_or_park`). `fire()` claims each task's event_id
    /// before the envelope becomes visible anywhere else, making this ticker the
    /// sole delivery owner of its fires — any other consumer that later observes
    /// the same envelope is a structural no-op.
    delivery_claims: DeliveryClaimRegistry,
    /// Memory Transparency Slice M4 (`memory.hygiene`) context. `None` when
    /// `with_memory_hygiene` was never called (tests, or a hotel with no
    /// `memory-hygiene:*` job registered) — `fire()` logs and skips instead
    /// of panicking if a job somehow targets [`crate::memory_hygiene::CRON_TARGET_ROLE`]
    /// without it.
    memory_hygiene: Option<MemoryHygieneCronContext>,
    /// Nightly dream-sweep (consolidation) context. Same shape and
    /// fleet-safety rules as `memory_hygiene`; `None` when
    /// `with_dream_sweep` was never called.
    dream_sweep: Option<DreamSweepCronContext>,
    /// Autopoiesis Slice A9 outcome-stamping follow-up
    /// (`crate::autonomy_sweep`) context. `None` when `with_autonomy_sweep`
    /// was never called — `fire()` logs and skips instead of panicking if a
    /// job somehow targets [`crate::autonomy_sweep::CRON_TARGET_ROLE`]
    /// without it.
    autonomy_sweep: Option<AutonomySweepCronContext>,
    /// Autopoiesis Slice A4 (`aria-architect-charter`) fire-time gate.
    /// `None` when `with_architect_charter` was never called. Unlike
    /// `memory_hygiene`/`dream_sweep`, this does NOT intercept delivery —
    /// the charter's daily fire goes through the normal role-delivery path
    /// below (PR #80 heritage). It only gates *whether* that normal path
    /// runs at all for a `architect-charter:*` job id — see the "Fire-time
    /// re-check" docs on `crate::architect_charter`.
    architect_charter: Option<ArchitectCharterCronContext>,
}

/// Wiring the in-process nightly dream sweep needs at fire time. Mirrors
/// [`MemoryHygieneCronContext`], including the load-bearing local opt-in
/// re-check (see that struct's `enabled_locally` doc for why job presence
/// alone is not consent once CronJobSync replicates definitions mesh-wide).
struct DreamSweepCronContext {
    muninn_config: Option<Arc<memory_core::MuninnConfig>>,
    hotel_name: String,
    /// This hotel's own `PHILOTIC_DREAM_SWEEP_ENABLED`, captured at boot.
    enabled_locally: bool,
}

/// Wiring the in-process `memory.hygiene` sweep needs at fire time — separate
/// from the constructor so existing `CronTicker::new` call sites (including
/// every test) are untouched; opt in via [`CronTicker::with_memory_hygiene`].
struct MemoryHygieneCronContext {
    muninn_config: Option<Arc<memory_core::MuninnConfig>>,
    hotel_name: String,
    intel_graph_url: Option<String>,
    /// This hotel's own `PHILOTIC_MEMORY_HYGIENE_ENABLED` opt-in, captured
    /// once at boot. **Load-bearing, not redundant with job registration:**
    /// `startup_sync`/`CronJobSync` replicate a `CronJob` *definition* to
    /// every mesh-connected peer unconditionally (`handle_cron_job_sync`
    /// upserts without checking any local flag), so a job registered on one
    /// opted-in hotel becomes locally due — and would otherwise locally
    /// fire — on every peer too. Re-checking the *local* opt-in at fire time
    /// is what actually keeps "operator opts in per hotel" true once the
    /// mesh is involved; without it, one hotel's opt-in silently sweeps the
    /// whole fleet.
    enabled_locally: bool,
    /// A9 Piece 3: lets a fresh hygiene filing push an unresolved
    /// pending-outcome breadcrumb into the same heal queue the A3
    /// heal-pattern-filing site already uses for operator visibility.
    /// `None` when no heal queue is configured on this hotel — the sweep
    /// still files the `autonomy_audit` record either way; this is
    /// visibility-only, best-effort.
    heal_queue: Option<Arc<dyn ansible_mesh_core::heal_queue::HealQueueStorage>>,
}

/// Wiring the in-process A9 outcome-stamping timeout-to-Neutral sweep needs
/// at fire time (`crate::autonomy_sweep`). Deliberately no `enabled_locally`
/// flag, unlike [`MemoryHygieneCronContext`]/[`DreamSweepCronContext`] — this
/// sweep is always-on, so the mesh-trap gate in
/// [`CronTicker::fire_autonomy_sweep`] compares the fired job's id against
/// this hotel's own deterministic job id instead (see `autonomy_sweep`
/// module docs).
struct AutonomySweepCronContext {
    hotel_name: String,
}

/// Wiring the Autopoiesis A4 architect-charter fire-time gate needs. Unlike
/// [`MemoryHygieneCronContext`]/[`DreamSweepCronContext`] (whose action is
/// symmetric — any hotel firing acts on itself), the charter delivers to a
/// specific `role:{agent_id}:{role_name}` that may only exist/mean something
/// on the hotel that registered it, so gating is job-id-scoped, not just
/// target-role-scoped. See `crate::architect_charter` module docs
/// ("Fire-time re-check") for the full reasoning.
struct ArchitectCharterCronContext {
    /// This hotel's own deterministic job id
    /// (`crate::architect_charter::cron_job_id(hotel_name)`). A fired job
    /// whose id does not match this exactly is a mesh-replicated peer's
    /// charter job and is never acted on locally, regardless of
    /// `enabled_locally`.
    expected_job_id: String,
    /// This hotel's own `PHILOTIC_ARCHITECT_CHARTER_ENABLED` (+ agent)
    /// opt-in, captured at boot.
    enabled_locally: bool,
}

impl CronTicker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        graph: Arc<GraphDomain>,
        dispatcher_tx: mpsc::Sender<LedgerCommand>,
        inboxes: InboxRegistry,
        local_node_id: impl Into<String>,
        offset_ms: u64,
        parked_inbound: ParkedInboundRegistry,
        materialization_requester: Option<Arc<dyn GuestMaterializationRequester>>,
        delivery_claims: DeliveryClaimRegistry,
    ) -> Self {
        Self {
            graph,
            dispatcher_tx,
            inboxes,
            local_node_id: local_node_id.into(),
            offset_ms,
            parked_inbound,
            materialization_requester,
            delivery_claims,
            memory_hygiene: None,
            dream_sweep: None,
            autonomy_sweep: None,
            architect_charter: None,
        }
    }

    /// Wire the `memory.hygiene` sweep context (Memory Transparency Slice
    /// M4). `fire()` intercepts jobs whose `target_role` is
    /// `crate::memory_hygiene::CRON_TARGET_ROLE` and runs the sweep
    /// in-process instead of delivering to a guest inbox — but only when
    /// `enabled_locally` is true (this hotel's own
    /// `PHILOTIC_MEMORY_HYGIENE_ENABLED`, not just a locally-present job
    /// record — see [`MemoryHygieneCronContext::enabled_locally`]).
    #[allow(clippy::too_many_arguments)]
    pub fn with_memory_hygiene(
        mut self,
        muninn_config: Option<Arc<memory_core::MuninnConfig>>,
        hotel_name: impl Into<String>,
        intel_graph_url: Option<String>,
        enabled_locally: bool,
        heal_queue: Option<Arc<dyn ansible_mesh_core::heal_queue::HealQueueStorage>>,
    ) -> Self {
        self.memory_hygiene = Some(MemoryHygieneCronContext {
            muninn_config,
            hotel_name: hotel_name.into(),
            intel_graph_url,
            enabled_locally,
            heal_queue,
        });
        self
    }

    /// Wire the A9 outcome-stamping timeout-to-Neutral sweep context
    /// (`crate::autonomy_sweep`). `fire()` intercepts jobs whose
    /// `target_role` is `crate::autonomy_sweep::CRON_TARGET_ROLE` and runs
    /// the sweep in-process. Unlike `with_memory_hygiene`/`with_dream_sweep`
    /// there is no `enabled_locally` flag to pass — this sweep is always-on;
    /// see [`AutonomySweepCronContext`] and `crate::autonomy_sweep` module
    /// docs for the mesh-trap gate this implies at fire time.
    pub fn with_autonomy_sweep(mut self, hotel_name: impl Into<String>) -> Self {
        self.autonomy_sweep = Some(AutonomySweepCronContext {
            hotel_name: hotel_name.into(),
        });
        self
    }

    /// Wire the nightly dream-sweep (consolidation) context. `fire()`
    /// intercepts jobs targeting `crate::dream::CRON_TARGET_ROLE` and runs
    /// the sweep in-process — but only when `enabled_locally` is true (this
    /// hotel's own `PHILOTIC_DREAM_SWEEP_ENABLED`, not just a
    /// mesh-replicated job record).
    pub fn with_dream_sweep(
        mut self,
        muninn_config: Option<Arc<memory_core::MuninnConfig>>,
        hotel_name: impl Into<String>,
        enabled_locally: bool,
    ) -> Self {
        self.dream_sweep = Some(DreamSweepCronContext {
            muninn_config,
            hotel_name: hotel_name.into(),
            enabled_locally,
        });
        self
    }

    /// Wire the Autopoiesis A4 architect-charter fire-time gate.
    /// `enabled_locally` is this hotel's own `PHILOTIC_ARCHITECT_CHARTER_ENABLED`
    /// **and** `PHILOTIC_ARCHITECT_CHARTER_AGENT` opt-in (both required —
    /// see `crate::architect_charter::ensure_scheduled`, which registers
    /// nothing without an explicit agent). `fire()` computes this hotel's own
    /// expected job id from `hotel_name` and only ever runs the normal
    /// role-delivery path for a `architect-charter:*` job whose id matches it
    /// exactly.
    pub fn with_architect_charter(
        mut self,
        hotel_name: impl Into<String>,
        enabled_locally: bool,
    ) -> Self {
        let hotel_name = hotel_name.into();
        self.architect_charter = Some(ArchitectCharterCronContext {
            expected_job_id: crate::architect_charter::cron_job_id(&hotel_name),
            enabled_locally,
        });
        self
    }

    /// Resolve a cron job's `target_role` (expected shape: `role:{agent_id}:{role_name}`,
    /// matching `RoleIncarnationRecord::routing_role`) to the role incarnation it names,
    /// if one is configured.
    fn resolve_target_role_record(
        graph: &GraphDomain,
        target_role: &str,
    ) -> Option<RoleIncarnationRecord> {
        let (agent_id, role_name) = target_role.strip_prefix("role:")?.split_once(':')?;
        graph
            .get_role_incarnation(agent_id, role_name)
            .ok()
            .flatten()
    }

    pub async fn run(self) {
        info!("CronTicker: started (offset_ms={})", self.offset_ms);

        // Slice 3: advertise local job definitions to mesh peers.
        self.startup_sync().await;

        // Slice 2: fire any missed guaranteed jobs before entering the tick loop.
        self.recovery_scan().await;

        let mut tick = interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            let now_ms = now_ms();

            // Non-guaranteed: fire-or-skip, no offset.
            let non_guaranteed = match self.graph.list_due_cron_jobs(now_ms, 0) {
                Ok(jobs) => jobs
                    .into_iter()
                    .filter(|j| !j.guaranteed)
                    .collect::<Vec<_>>(),
                Err(e) => {
                    error!("CronTicker: list_due_cron_jobs (non-guaranteed) failed: {e}");
                    continue;
                }
            };

            // Guaranteed: staggered by hotel offset.
            let guaranteed = match self.graph.list_due_cron_jobs(now_ms, self.offset_ms) {
                Ok(jobs) => jobs
                    .into_iter()
                    .filter(|j| j.guaranteed)
                    .collect::<Vec<_>>(),
                Err(e) => {
                    error!("CronTicker: list_due_cron_jobs (guaranteed) failed: {e}");
                    continue;
                }
            };

            for job in non_guaranteed.into_iter().chain(guaranteed) {
                self.fire(&job, now_ms).await;
            }
        }
    }

    /// Broadcast all locally-registered cron jobs as `CronJobSync` upsert events
    /// so that peer hotels replicate the definitions on connect.
    async fn startup_sync(&self) {
        let jobs = match self.graph.list_cron_jobs() {
            Ok(j) => j,
            Err(e) => {
                error!("CronTicker: startup_sync failed to list jobs: {e}");
                return;
            }
        };

        if jobs.is_empty() {
            return;
        }

        let now_ms = now_ms();
        for job in &jobs {
            self.broadcast_cron_job_sync_upsert(job, now_ms).await;
        }
        info!("CronTicker: startup_sync broadcast {} job(s)", jobs.len());
    }

    /// On startup, fire any guaranteed jobs whose epoch was missed while the
    /// hotel was down — unless another hotel already handled them.
    async fn recovery_scan(&self) {
        let now_ms = now_ms();
        let jobs = match self.graph.list_cron_jobs() {
            Ok(j) => j,
            Err(e) => {
                error!("CronTicker: recovery scan failed to list jobs: {e}");
                return;
            }
        };

        for job in jobs {
            if !job.enabled || !job.guaranteed {
                continue;
            }
            if job.next_fire_at >= now_ms {
                continue;
            }
            if job.last_fired_epoch == Some(job.next_fire_at) {
                // Already handled; advance schedule.
                if let Err(e) = self.advance_schedule(&job, job.next_fire_at) {
                    error!(
                        "CronTicker: recovery advance failed for job {}: {e}",
                        job.id
                    );
                }
                continue;
            }
            info!(
                "CronTicker: recovery fire for job {} (epoch={})",
                job.id, job.next_fire_at
            );
            self.fire(&job, now_ms).await;
        }
    }

    async fn fire(&self, job: &CronJob, now_ms: u64) {
        let fire_epoch = job.next_fire_at;

        // Memory Transparency Slice M4 (`memory.hygiene`): this sentinel
        // target_role never resolves to a guest inbox — the sweep runs
        // in-process, right here, instead of going through TaskInvoke
        // ledger/delivery/materialization. No guaranteed-dedup semantics
        // apply (the job is always non-guaranteed): the schedule still
        // advances below so the next fire lands on the following occurrence.
        if job.target_role == crate::memory_hygiene::CRON_TARGET_ROLE {
            self.fire_memory_hygiene(now_ms).await;
            if let Err(e) = self.advance_schedule(job, fire_epoch) {
                error!(
                    "CronTicker: memory.hygiene advance failed for job {}: {e}",
                    job.id
                );
            }
            return;
        }

        // Nightly dream sweep: same in-process sentinel interception as
        // memory.hygiene — consolidation must not wait for hotel shutdown.
        if job.target_role == crate::dream::CRON_TARGET_ROLE {
            self.fire_dream_sweep().await;
            if let Err(e) = self.advance_schedule(job, fire_epoch) {
                error!(
                    "CronTicker: dream-sweep advance failed for job {}: {e}",
                    job.id
                );
            }
            return;
        }

        // Autopoiesis Slice A9 outcome-stamping follow-up: same in-process
        // sentinel interception, gated on this hotel's own job id rather than
        // a local-enabled flag (see `fire_autonomy_sweep` and
        // `crate::autonomy_sweep` module docs — this sweep has no opt-in).
        if job.target_role == crate::autonomy_sweep::CRON_TARGET_ROLE {
            self.fire_autonomy_sweep(job, now_ms).await;
            if let Err(e) = self.advance_schedule(job, fire_epoch) {
                error!(
                    "CronTicker: autonomy_sweep advance failed for job {}: {e}",
                    job.id
                );
            }
            return;
        }

        // Autopoiesis Slice A4 (`aria-architect-charter`): unlike the
        // in-process sentinels above, a charter job DOES go through the
        // normal role-delivery path below — but only for THIS hotel's own
        // registration. `CronJobSync` replicates job definitions mesh-wide
        // unconditionally, so a peer hotel's `architect-charter:<peer>` job
        // can become locally due here too; without this re-check this hotel
        // would attempt to deliver to a `role:{agent}:{role}` that may not
        // exist (or may mean something different) on this hotel. See
        // `crate::architect_charter` module docs ("Fire-time re-check").
        if job.id.starts_with(crate::architect_charter::JOB_ID_PREFIX) {
            let allowed = self
                .architect_charter
                .as_ref()
                .is_some_and(|ctx| ctx.enabled_locally && job.id == ctx.expected_job_id);
            if !allowed {
                debug!(
                    job_id = %job.id,
                    "CronTicker: architect-charter job not enabled locally for this hotel/job id \
                     — skipping fire (likely a mesh-replicated peer registration)"
                );
                if let Err(e) = self.advance_schedule(job, fire_epoch) {
                    error!(
                        "CronTicker: architect-charter advance failed for job {}: {e}",
                        job.id
                    );
                }
                return;
            }
        }

        // Guaranteed dedup: if last_fired_epoch already covers this epoch,
        // another hotel fired it; skip and advance our local schedule.
        if job.guaranteed && job.last_fired_epoch == Some(fire_epoch) {
            if let Err(e) = self.advance_schedule(job, fire_epoch) {
                error!("CronTicker: dedup advance failed for job {}: {e}", job.id);
            }
            return;
        }

        // Slice 4: interpolate payload template variables.
        let vars =
            CronInterpolationVars::new(fire_epoch, &job.id, &self.local_node_id, &job.target_role);
        let payload_data = interpolate_payload(&job.payload, &vars);

        let task_id = Uuid::new_v4();
        let task_json = build_cron_task_json(job, fire_epoch, &self.local_node_id, payload_data);

        // Single-delivery ownership: claim the event id BEFORE the envelope is
        // visible to any other consumer. `fire()` is the sole delivery owner of
        // this task; the mesh/ledger consumer (`deliver_event_envelope_or_park`)
        // consults the same claim set and skips already-claimed event ids, so
        // double delivery is structurally impossible even if this envelope loops
        // back through a mesh batch or ledger replay (session-18 dual-consumer race).
        if !crate::service::ipc::claim_delivery(&self.delivery_claims, task_id) {
            warn!(
                "CronTicker: event id {} for job {} already claimed; skipping duplicate delivery",
                task_id, job.id
            );
            return;
        }

        // Durably append TaskInvoke to the event ledger.
        let task_env = EventEnvelope {
            event_id: task_id,
            seq: 0,
            source_node_id: self.local_node_id.clone(),
            target_node_id: Some(self.local_node_id.clone()),
            source_agent_id: "cron-ticker".into(),
            target_agent_id: Some(job.target_role.clone()),
            kind: EventKind::TaskInvoke,
            corr_id: format!("cron:{}", job.id),
            attempt: 0,
            created_at: now_ms,
            expires_at: None,
            payload: EventPayload::Inline {
                data: task_json.clone(),
            },
            trace: vec![format!("cron-ticker:{}", job.id)],
        };

        if let Err(e) = self
            .dispatcher_tx
            .send(LedgerCommand::AppendLocal(task_env))
            .await
        {
            error!(
                "CronTicker: failed to append TaskInvoke for job {}: {e}",
                job.id
            );
            return;
        }

        // Deliver to the role inbox (local delivery). If the target role resolves to a
        // configured role incarnation that isn't currently subscribed (e.g. an on-demand
        // role guest that hasn't been spawned yet), park the task and trigger its
        // materialization instead of dropping it ledger-only.
        let target_role_record = Self::resolve_target_role_record(&self.graph, &job.target_role);
        let is_subscribed = {
            let guard = self.inboxes.lock().await;
            let role_subs = guard
                .get(job.target_role.as_str())
                .cloned()
                .unwrap_or_default();
            match &target_role_record {
                Some(record) => role_subs.iter().any(|s| s.guest_id == record.guest_id),
                None => !role_subs.is_empty(),
            }
        };

        match (&target_role_record, is_subscribed) {
            (Some(record), false) => {
                crate::service::ipc::IpcServer::park_and_materialize(
                    &self.graph,
                    &self.inboxes,
                    &self.parked_inbound,
                    self.materialization_requester.as_deref(),
                    &self.local_node_id,
                    &self.local_node_id,
                    task_id,
                    task_json,
                    crate::service::ipc::ParkTarget::LocalRoleIncarnation {
                        role_record: record,
                    },
                )
                .await;
            }
            _ => {
                crate::service::ipc::IpcServer::deliver_inbound_task(
                    &self.inboxes,
                    &self.local_node_id,
                    &job.target_role,
                    target_role_record.as_ref().map(|r| r.guest_id.as_str()),
                    task_id,
                    task_json,
                )
                .await;
            }
        }

        info!(
            "CronTicker: fired job {} → role={} epoch={}",
            job.id, job.target_role, fire_epoch
        );

        // Guaranteed: broadcast CronFired so peer hotels suppress their offset fire.
        if job.guaranteed {
            self.broadcast_cron_fired(job, fire_epoch, now_ms).await;
        }

        if let Err(e) = self.advance_schedule(job, fire_epoch) {
            error!(
                "CronTicker: failed to advance schedule for job {}: {e}",
                job.id
            );
        }
    }

    /// Run the `memory.hygiene` sweep in-process (Memory Transparency Slice
    /// M4). Logs and returns if `with_memory_hygiene` was never called —
    /// should not happen in practice since `ensure_scheduled` only registers
    /// the job when the hotel has opted in, but a defensive no-op beats a
    /// panic on a cron tick.
    async fn fire_memory_hygiene(&self, now_ms: u64) {
        let Some(ctx) = &self.memory_hygiene else {
            warn!(
                "CronTicker: memory.hygiene job fired but no context was wired \
                 (with_memory_hygiene not called) — skipping"
            );
            return;
        };
        // Re-check this hotel's own opt-in, not just whether the job record
        // exists locally: mesh `CronJobSync` replicates job *definitions* to
        // every peer unconditionally, so this fire may be for a job an
        // operator enabled on a *different* hotel. Without this check one
        // hotel's opt-in would silently sweep every mesh-connected peer.
        if !ctx.enabled_locally {
            debug!(
                "CronTicker: memory.hygiene job fired but this hotel has not opted in \
                 (PHILOTIC_MEMORY_HYGIENE_ENABLED unset here) — job definition was likely \
                 replicated via CronJobSync from a peer hotel; skipping sweep"
            );
            return;
        }
        crate::memory_hygiene::run_scheduled_sweep(
            &self.graph,
            ctx.muninn_config.as_deref(),
            &ctx.hotel_name,
            ctx.intel_graph_url.as_deref(),
            ctx.heal_queue.as_deref(),
            now_ms / 1000,
        )
        .await;
    }

    /// Run the dream (consolidation) sweep in-process. Same defensive no-op
    /// and fleet-safety local-opt-in re-check as [`Self::fire_memory_hygiene`].
    async fn fire_dream_sweep(&self) {
        let Some(ctx) = &self.dream_sweep else {
            warn!(
                "CronTicker: dream-sweep job fired but no context was wired \
                 (with_dream_sweep not called) — skipping"
            );
            return;
        };
        if !ctx.enabled_locally {
            debug!(
                "CronTicker: dream-sweep job fired but this hotel has not opted in \
                 (PHILOTIC_DREAM_SWEEP_ENABLED unset here) — job definition was likely \
                 replicated via CronJobSync from a peer hotel; skipping sweep"
            );
            return;
        }
        let Some(config) = &ctx.muninn_config else {
            debug!("CronTicker: dream-sweep fired but MuninnDB is not configured — skipping");
            return;
        };
        crate::dream::dream_sweep(config, &self.graph, &ctx.hotel_name).await;
    }

    /// Run the A9 outcome-stamping timeout-to-Neutral sweep in-process
    /// (`crate::autonomy_sweep`). Logs and returns if `with_autonomy_sweep`
    /// was never called — should not happen in practice since
    /// `autonomy_sweep::ensure_scheduled` runs unconditionally at boot, but a
    /// defensive no-op beats a panic on a cron tick.
    ///
    /// **Mesh trap (load-bearing):** unlike [`Self::fire_memory_hygiene`]/
    /// [`Self::fire_dream_sweep`], there is no `enabled_locally` flag to
    /// re-check — this sweep has no opt-in. Instead the gate is the fired
    /// job's id: `CronJobSync` replicates job *definitions* to every
    /// mesh-connected peer unconditionally, so a job with a peer hotel's
    /// deterministic id (`autonomy-outcome-sweep:{peer}`) can become locally
    /// due here too. Only a job whose id matches *this* hotel's own
    /// (`crate::autonomy_sweep::cron_job_id(&ctx.hotel_name)`) is swept —
    /// anything else is a replicated definition and is silently skipped, so
    /// each hotel only ever sweeps its own audit records under its own name.
    async fn fire_autonomy_sweep(&self, job: &CronJob, now_ms: u64) {
        let Some(ctx) = &self.autonomy_sweep else {
            warn!(
                "CronTicker: autonomy_sweep job fired but no context was wired \
                 (with_autonomy_sweep not called) — skipping"
            );
            return;
        };
        let own_job_id = crate::autonomy_sweep::cron_job_id(&ctx.hotel_name);
        if job.id != own_job_id {
            debug!(
                job_id = %job.id,
                own_job_id = %own_job_id,
                hotel = %ctx.hotel_name,
                "CronTicker: autonomy_sweep job fired but its id does not match this hotel's \
                 own job — job definition was likely replicated via CronJobSync from a peer \
                 hotel; skipping sweep"
            );
            return;
        }
        crate::autonomy_sweep::run_scheduled_sweep(&self.graph, &ctx.hotel_name, now_ms / 1000);
    }

    async fn broadcast_cron_fired(&self, job: &CronJob, fire_epoch: u64, now_ms: u64) {
        let payload = serde_json::json!({
            "job_id": job.id,
            "fire_epoch": fire_epoch,
            "fired_by": self.local_node_id,
        })
        .to_string();

        let env = EventEnvelope {
            event_id: Uuid::new_v4(),
            seq: 0,
            source_node_id: self.local_node_id.clone(),
            target_node_id: None,
            source_agent_id: "cron-ticker".into(),
            target_agent_id: None,
            kind: EventKind::CronFired,
            corr_id: format!("cron-fired:{}:{}", job.id, fire_epoch),
            attempt: 0,
            created_at: now_ms,
            expires_at: None,
            payload: EventPayload::Inline { data: payload },
            trace: vec![format!("cron-ticker:{}", job.id)],
        };

        if let Err(e) = self
            .dispatcher_tx
            .send(LedgerCommand::AppendLocal(env))
            .await
        {
            warn!(
                "CronTicker: failed to broadcast CronFired for job {}: {e}",
                job.id
            );
        }
    }

    /// Broadcast a `CronJobSync` upsert so peer hotels replicate this job definition.
    pub(crate) async fn broadcast_cron_job_sync_upsert(&self, job: &CronJob, now_ms: u64) {
        let payload = serde_json::json!({
            "op": "upsert",
            "job": job,
        })
        .to_string();

        let env = EventEnvelope {
            event_id: Uuid::new_v4(),
            seq: 0,
            source_node_id: self.local_node_id.clone(),
            target_node_id: None,
            source_agent_id: "cron-ticker".into(),
            target_agent_id: None,
            kind: EventKind::CronJobSync,
            corr_id: format!("cron-sync:{}", job.id),
            attempt: 0,
            created_at: now_ms,
            expires_at: None,
            payload: EventPayload::Inline { data: payload },
            trace: vec![format!("cron-sync:{}", job.id)],
        };

        if let Err(e) = self
            .dispatcher_tx
            .send(LedgerCommand::AppendLocal(env))
            .await
        {
            warn!(
                "CronTicker: failed to broadcast CronJobSync upsert for {}: {e}",
                job.id
            );
        }
    }

    /// Broadcast a `CronJobSync` remove so peer hotels drop this job definition.
    pub(crate) async fn broadcast_cron_job_sync_remove(&self, job_id: &str, now_ms: u64) {
        let payload = serde_json::json!({
            "op": "remove",
            "job_id": job_id,
        })
        .to_string();

        let env = EventEnvelope {
            event_id: Uuid::new_v4(),
            seq: 0,
            source_node_id: self.local_node_id.clone(),
            target_node_id: None,
            source_agent_id: "cron-ticker".into(),
            target_agent_id: None,
            kind: EventKind::CronJobSync,
            corr_id: format!("cron-sync-remove:{}", job_id),
            attempt: 0,
            created_at: now_ms,
            expires_at: None,
            payload: EventPayload::Inline { data: payload },
            trace: vec![format!("cron-sync-remove:{}", job_id)],
        };

        if let Err(e) = self
            .dispatcher_tx
            .send(LedgerCommand::AppendLocal(env))
            .await
        {
            warn!(
                "CronTicker: failed to broadcast CronJobSync remove for {}: {e}",
                job_id
            );
        }
    }

    fn advance_schedule(&self, job: &CronJob, fire_epoch: u64) -> anyhow::Result<()> {
        let next = next_fire_after(&job.schedule, fire_epoch).unwrap_or_else(|e| {
            warn!(
                "CronTicker: no future occurrences for job {} ({}); disabling",
                job.id, e
            );
            u64::MAX
        });

        let mut updated = job.clone();
        updated.last_fired_epoch = Some(fire_epoch);

        if next == u64::MAX {
            updated.enabled = false;
        } else {
            updated.next_fire_at = next;
        }

        self.graph.upsert_cron_job(&updated)
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Overrides `task["session_id"]` for `CronSessionTarget::Isolated` jobs so
/// the fire lands in its own `cron:<job_id>` session instead of whatever
/// (if anything) the payload named. Philote checkpoints its rolling turn
/// window under `short_session:{session_id}`, so a shared session id would
/// let a cron fire evict real conversational turns — the Beacon Chronos
/// context-pollution incident (2026-07-02) this proposal closes. `Main`
/// jobs (the legacy marker) leave any payload-supplied `session_id`
/// untouched, preserving today's behavior.
fn apply_cron_session_routing(mut task: serde_json::Value, job: &CronJob) -> serde_json::Value {
    if job.session_target == ansible_mesh_core::cron::CronSessionTarget::Isolated {
        task["session_id"] =
            serde_json::Value::String(ansible_mesh_core::cron::cron_session_id(&job.id));
    }
    task
}

fn build_cron_task_json(
    job: &CronJob,
    fire_epoch: u64,
    local_node_id: &str,
    payload_data: String,
) -> String {
    let Ok(payload_json) = serde_json::from_str::<serde_json::Value>(&payload_data) else {
        let task = serde_json::json!({
            "cron_job_id": job.id,
            "target_role": job.target_role,
            "fire_epoch": fire_epoch,
            "payload": payload_data,
        });
        return apply_cron_session_routing(task, job).to_string();
    };

    let Some(signal_seed) = payload_json.get("paracrine_signal") else {
        // Promote routing fields so the receiving agent has content and can
        // route its reply — without `content`, normalized_user_content returns
        // None and handle_user_message silently drops the task.
        let content = payload_json
            .get("message")
            .or_else(|| payload_json.get("content"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let mut obj = serde_json::json!({
            "cron_job_id": job.id,
            "target_role": job.target_role,
            "fire_epoch": fire_epoch,
            "payload": payload_data,
        });
        if let Some(c) = content {
            obj["content"] = serde_json::Value::String(c.to_string());
        }
        for key in ["source", "chat_id", "session_id"] {
            if let Some(v) = payload_json.get(key) {
                obj[key] = v.clone();
            }
        }
        // Operator-authored jobs may carry a narrow standing tool
        // preapproval: the operator approved these tools when they authored
        // the job's payload, so the receiving philote seeds them into the
        // cron session's approval policy instead of parking an unattended
        // turn as WaitingApproval. NEVER forwarded for guest-created jobs —
        // a guest could otherwise register a cron job that self-grants
        // approval for high-agency tools (privilege escalation).
        if matches!(
            job.created_by,
            ansible_mesh_core::cron::CronJobSource::Operator
        ) {
            if let Some(tools) = payload_json
                .get("preapproved_tools")
                .and_then(serde_json::Value::as_array)
            {
                let clean: Vec<serde_json::Value> = tools
                    .iter()
                    .filter(|t| t.as_str().is_some_and(|s| !s.trim().is_empty()))
                    .cloned()
                    .collect();
                if !clean.is_empty() {
                    obj["cron_preapproved_tools"] = serde_json::Value::Array(clean);
                }
            }
        }
        return apply_cron_session_routing(obj, job).to_string();
    };

    let mut signal = signal_seed
        .as_object()
        .cloned()
        .unwrap_or_else(serde_json::Map::new);
    signal
        .entry("signal_id")
        .or_insert_with(|| serde_json::Value::String(format!("cron:{}:{fire_epoch}", job.id)));
    signal
        .entry("signal_type")
        .or_insert_with(|| serde_json::Value::String("heartbeat".into()));
    signal
        .entry("scope")
        .or_insert_with(|| serde_json::Value::String("hotel".into()));
    signal
        .entry("source_node")
        .or_insert_with(|| serde_json::Value::String(local_node_id.to_string()));
    signal
        .entry("source_hotel")
        .or_insert_with(|| serde_json::Value::String(local_node_id.to_string()));
    signal
        .entry("target_role_type")
        .or_insert_with(|| serde_json::Value::String(job.target_role.clone()));
    signal
        .entry("cadence")
        .or_insert_with(|| serde_json::Value::String(job.schedule.clone()));
    signal
        .entry("priority")
        .or_insert_with(|| serde_json::Value::String("normal".into()));
    signal
        .entry("observed_at")
        .or_insert_with(|| serde_json::Value::Number(fire_epoch.into()));
    signal
        .entry("policy_tags")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    signal
        .entry("subject_refs")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));

    let task = serde_json::json!({
        "action": "paracrine_signal",
        "source": "cron-ticker",
        "transport": "cron",
        "cron_job_id": job.id,
        "target_role": job.target_role,
        "fire_epoch": fire_epoch,
        "payload": payload_json,
        "paracrine_signal": serde_json::Value::Object(signal),
    });
    apply_cron_session_routing(task, job).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::cron::CronJobSource;

    fn test_job() -> CronJob {
        CronJob {
            id: "job-1".into(),
            schedule: "0 */15 * * * * *".into(),
            target_role: "attention-steward".into(),
            target_node_id: None,
            payload: "{}".into(),
            guaranteed: false,
            enabled: true,
            last_fired_epoch: None,
            next_fire_at: 1_000,
            created_at: 900,
            created_by: CronJobSource::Operator,
            silent_ok: false,
            session_target: ansible_mesh_core::cron::CronSessionTarget::Main,
        }
    }

    #[test]
    fn cron_legacy_payload_with_message_promotes_content_and_routing_fields() {
        let task = build_cron_task_json(
            &test_job(),
            1_234,
            "vps-jane-aiua-01",
            r#"{"message":"Good evening — time for your check-in","source":"telegram","chat_id":7898847424,"session_id":"telegram:7898847424:agent-beacon"}"#.into(),
        );
        let value: serde_json::Value = serde_json::from_str(&task).unwrap();

        assert_eq!(
            value["content"], "Good evening — time for your check-in",
            "message must be promoted to top-level content"
        );
        assert_eq!(value["source"], "telegram");
        assert_eq!(value["chat_id"], 7898847424i64);
        assert_eq!(value["session_id"], "telegram:7898847424:agent-beacon");
        assert_eq!(value["cron_job_id"], "job-1");
        assert!(
            value.get("action").is_none(),
            "action must be absent for non-paracrine tasks"
        );
    }

    #[test]
    fn operator_job_forwards_preapproved_tools() {
        let task = build_cron_task_json(
            &test_job(), // created_by: Operator
            1_234,
            "mbp-jane-aiua-01",
            r#"{"message":"run the backup","preapproved_tools":["bash.exec","","  "]}"#.into(),
        );
        let value: serde_json::Value = serde_json::from_str(&task).unwrap();
        assert_eq!(
            value["cron_preapproved_tools"],
            serde_json::json!(["bash.exec"]),
            "operator-authored preapproval must be forwarded (blank entries dropped)"
        );
    }

    /// A guest-created job must NEVER forward preapproved_tools — a guest
    /// could otherwise register a cron job that self-grants approval for
    /// high-agency tools.
    #[test]
    fn guest_job_never_forwards_preapproved_tools() {
        let mut job = test_job();
        job.created_by = CronJobSource::Guest("agent-aria".into());
        let task = build_cron_task_json(
            &job,
            1_234,
            "mbp-jane-aiua-01",
            r#"{"message":"run the backup","preapproved_tools":["bash.exec"]}"#.into(),
        );
        let value: serde_json::Value = serde_json::from_str(&task).unwrap();
        assert!(
            value.get("cron_preapproved_tools").is_none(),
            "guest-created jobs must not self-grant tool approval"
        );
    }

    #[test]
    fn cron_payload_without_paracrine_signal_keeps_legacy_task_shape() {
        let task = build_cron_task_json(
            &test_job(),
            1_234,
            "mac-jane-aiua-01",
            r#"{"hello":"world"}"#.into(),
        );
        let value: serde_json::Value = serde_json::from_str(&task).unwrap();

        assert_eq!(value["cron_job_id"], "job-1");
        assert_eq!(value["target_role"], "attention-steward");
        assert_eq!(value["fire_epoch"], 1_234);
        assert_eq!(value["payload"], r#"{"hello":"world"}"#);
        assert!(value.get("action").is_none());
    }

    #[test]
    fn cron_payload_with_paracrine_signal_builds_normalized_signal_task() {
        let task = build_cron_task_json(
            &test_job(),
            1_234,
            "mac-jane-aiua-01",
            r#"{
                "paracrine_signal": {
                    "signal_type": "life_graph.attention_scan",
                    "scope": "life_graph",
                    "policy_tags": ["observe_only"]
                },
                "payload_summary": "scan open loops"
            }"#
            .into(),
        );
        let value: serde_json::Value = serde_json::from_str(&task).unwrap();

        assert_eq!(value["action"], "paracrine_signal");
        assert_eq!(value["transport"], "cron");
        assert_eq!(value["cron_job_id"], "job-1");
        assert_eq!(value["paracrine_signal"]["signal_id"], "cron:job-1:1234");
        assert_eq!(
            value["paracrine_signal"]["signal_type"],
            "life_graph.attention_scan"
        );
        assert_eq!(value["paracrine_signal"]["source_node"], "mac-jane-aiua-01");
        assert_eq!(
            value["paracrine_signal"]["source_hotel"],
            "mac-jane-aiua-01"
        );
        assert_eq!(
            value["paracrine_signal"]["target_role_type"],
            "attention-steward"
        );
        assert_eq!(value["paracrine_signal"]["cadence"], "0 */15 * * * * *");
        assert_eq!(value["paracrine_signal"]["observed_at"], 1_234);
        assert_eq!(value["paracrine_signal"]["policy_tags"][0], "observe_only");
    }

    fn isolated_job() -> CronJob {
        let mut job = test_job();
        job.session_target = ansible_mesh_core::cron::CronSessionTarget::Isolated;
        job
    }

    #[test]
    fn isolated_session_target_overrides_payload_session_id_on_legacy_branch() {
        let task = build_cron_task_json(
            &isolated_job(),
            1_234,
            "vps-jane-aiua-01",
            r#"{"message":"Good evening — time for your check-in","source":"telegram","chat_id":7898847424,"session_id":"telegram:7898847424:agent-beacon"}"#.into(),
        );
        let value: serde_json::Value = serde_json::from_str(&task).unwrap();

        assert_eq!(
            value["session_id"], "cron:job-1",
            "Isolated jobs must never inherit a payload-supplied session_id"
        );
        // Everything else about the legacy branch is unchanged.
        assert_eq!(value["content"], "Good evening — time for your check-in");
        assert_eq!(value["source"], "telegram");
    }

    #[test]
    fn isolated_session_target_sets_session_id_when_payload_has_none() {
        let task = build_cron_task_json(
            &isolated_job(),
            1_234,
            "mac-jane-aiua-01",
            r#"{"hello":"world"}"#.into(),
        );
        let value: serde_json::Value = serde_json::from_str(&task).unwrap();
        assert_eq!(value["session_id"], "cron:job-1");
    }

    #[test]
    fn isolated_session_target_sets_session_id_on_paracrine_branch() {
        let task = build_cron_task_json(
            &isolated_job(),
            1_234,
            "mac-jane-aiua-01",
            r#"{
                "paracrine_signal": {
                    "signal_type": "life_graph.attention_scan",
                    "scope": "life_graph",
                    "policy_tags": ["observe_only"]
                },
                "payload_summary": "scan open loops"
            }"#
            .into(),
        );
        let value: serde_json::Value = serde_json::from_str(&task).unwrap();
        assert_eq!(value["session_id"], "cron:job-1");
    }

    #[test]
    fn isolated_session_target_sets_session_id_on_malformed_payload_branch() {
        let task = build_cron_task_json(
            &isolated_job(),
            1_234,
            "mac-jane-aiua-01",
            "not json".into(),
        );
        let value: serde_json::Value = serde_json::from_str(&task).unwrap();
        assert_eq!(value["session_id"], "cron:job-1");
    }

    #[test]
    fn main_session_target_never_injects_session_id_when_payload_has_none() {
        let task = build_cron_task_json(
            &test_job(),
            1_234,
            "mac-jane-aiua-01",
            r#"{"hello":"world"}"#.into(),
        );
        let value: serde_json::Value = serde_json::from_str(&task).unwrap();
        assert!(
            value.get("session_id").is_none(),
            "Main jobs must not gain a session_id that wasn't in the payload"
        );
    }

    #[tokio::test]
    async fn fire_parks_and_materializes_dormant_role_incarnation_instead_of_dropping_task() {
        use ansible_mesh_core::NodeCapabilities;
        use ansible_mesh_core::graph::{RoleReadinessState, TurnLoopConfig};
        use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
        use ansible_mesh_core::storage::HotelRecord;
        use async_trait::async_trait;
        use std::collections::HashMap;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::sync::Mutex;

        struct MockRequester {
            calls: AtomicUsize,
            last_guest_id: Mutex<Option<String>>,
        }

        #[async_trait]
        impl GuestMaterializationRequester for MockRequester {
            async fn ensure_guest_active(&self, guest_id: &str) -> anyhow::Result<bool> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                *self.last_guest_id.lock().await = Some(guest_id.to_string());
                Ok(true)
            }

            async fn restart_guest(&self, guest_id: &str) -> anyhow::Result<bool> {
                self.ensure_guest_active(guest_id).await
            }
        }

        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: "/tmp/cron-ticker-test.sock".into(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-test".into(),
                role_name: "orchestrator".into(),
                guest_id: "agent-test:orchestrator".into(),
                toolset_profile: "orchestrator".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: true,
                readiness_state: RoleReadinessState::Configured,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
            })
            .expect("seed role incarnation");

        let mut job = test_job();
        job.target_role = "role:agent-test:orchestrator".into();

        // Drained by a background task (see `ipc::test_dispatcher_channel`) so
        // ledger sends can never block, no matter how many entries fire pushes.
        let (dispatcher_tx, _dispatcher_rx) = crate::service::ipc::test_dispatcher_channel();
        let requester = Arc::new(MockRequester {
            calls: AtomicUsize::new(0),
            last_guest_id: Mutex::new(None),
        });
        let parked_inbound: crate::service::ipc::ParkedInboundRegistry =
            Arc::new(Mutex::new(HashMap::new()));
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));

        let ticker = CronTicker::new(
            graph.clone(),
            dispatcher_tx,
            inboxes,
            "local-aiua-01",
            0,
            parked_inbound.clone(),
            Some(requester.clone() as Arc<dyn GuestMaterializationRequester>),
            crate::service::ipc::new_delivery_claim_registry(),
        );

        ticker.fire(&job, 1_000).await;

        assert_eq!(
            requester.calls.load(Ordering::SeqCst),
            1,
            "fire() should trigger on-demand materialization for the dormant role guest"
        );
        assert_eq!(
            requester.last_guest_id.lock().await.as_deref(),
            Some("agent-test:orchestrator"),
            "materialization must target the role incarnation's own guest_id, not a \
             cross-hotel philote-{{role}} placeholder"
        );

        let parked = parked_inbound.lock().await;
        assert_eq!(
            parked.get("agent-test:orchestrator").map(Vec::len),
            Some(1),
            "task should be parked under the role incarnation's own guest_id, not dropped"
        );

        let guest = graph
            .get_guest("local-hotel", "agent-test:orchestrator")
            .expect("get_guest should not error")
            .expect("on-demand materialization should have upserted the local role guest record");
        assert!(
            guest.is_active,
            "materialization must flip the dormant role guest active, not leave it dead"
        );
    }

    /// Session-18 regression: a fired cron job's `TaskInvoke` was observable by TWO
    /// independent consumers — `fire()`'s own delivery and the mesh/ledger consumer
    /// (`deliver_event_envelope_or_park`) reacting to the same envelope — and which
    /// one "won" varied per fire. This test simulates both consumers observing one
    /// fire and asserts the task reaches the subscriber exactly once: `fire()` claims
    /// the event id first, so the second consumer must be a structural no-op.
    #[tokio::test]
    async fn fired_cron_task_is_delivered_exactly_once_across_both_consumers() {
        use ansible_mesh_core::NodeCapabilities;
        use ansible_mesh_core::graph::{RoleReadinessState, TurnLoopConfig};
        use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
        use ansible_mesh_core::storage::HotelRecord;
        use philotic_client::IpcResponse;
        use std::collections::HashMap;
        use tokio::sync::Mutex;

        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: "/tmp/cron-single-consumer-test.sock".into(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-test".into(),
                role_name: "orchestrator".into(),
                guest_id: "agent-test:orchestrator".into(),
                toolset_profile: "orchestrator".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: true,
                readiness_state: RoleReadinessState::Configured,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
            })
            .expect("seed role incarnation");

        let mut job = test_job();
        job.target_role = "role:agent-test:orchestrator".into();

        // A LIVE subscriber for the role — the warm-guest path (watched-live-proven,
        // must not regress): fire() delivers directly instead of parking.
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));
        let (subscriber_tx, mut subscriber_rx) =
            tokio::sync::mpsc::unbounded_channel::<IpcResponse>();
        let mut subscribed_roles = Vec::new();
        crate::service::ipc::IpcServer::add_subscription(
            &inboxes,
            "role:agent-test:orchestrator",
            Uuid::new_v4(),
            "agent-test:orchestrator",
            &[],
            &crate::service::ipc::CountedSender::detached(&subscriber_tx),
            &mut subscribed_roles,
        )
        .await;

        let (dispatcher_tx, mut dispatcher_rx) = crate::service::ipc::test_dispatcher_channel();
        let parked_inbound: crate::service::ipc::ParkedInboundRegistry =
            Arc::new(Mutex::new(HashMap::new()));
        // The single claim set BOTH consumers consult.
        let delivery_claims = crate::service::ipc::new_delivery_claim_registry();

        let ticker = CronTicker::new(
            graph.clone(),
            dispatcher_tx,
            inboxes.clone(),
            "local-aiua-01",
            0,
            parked_inbound.clone(),
            None,
            delivery_claims.clone(),
        );

        // Consumer 1: the cron fire itself.
        ticker.fire(&job, 1_000).await;

        // Capture the exact envelope fire() appended to the ledger.
        let appended = tokio::time::timeout(Duration::from_secs(5), dispatcher_rx.recv())
            .await
            .expect("dispatcher channel should yield the appended command")
            .expect("dispatcher channel should stay open");
        let crate::LedgerCommand::AppendLocal(envelope) = appended else {
            panic!("fire() should append the TaskInvoke via AppendLocal");
        };
        assert!(
            matches!(envelope.kind, EventKind::TaskInvoke),
            "first appended command must be the fired TaskInvoke"
        );

        // Consumer 2: the mesh/ledger consumer observes the SAME envelope (echoed
        // batch / ledger replay). It must recognise the claim and deliver nothing.
        let handled = crate::service::ipc::IpcServer::deliver_event_envelope_or_park(
            &inboxes,
            &envelope,
            None,
            &graph,
            "local-aiua-01",
            &parked_inbound,
            None,
            &delivery_claims,
        )
        .await;
        assert!(
            handled,
            "second consumer should report the claimed event as handled"
        );

        let first = subscriber_rx.try_recv();
        assert!(
            matches!(first, Ok(IpcResponse::InboundTask { .. })),
            "the fire must reach the live subscriber exactly once (got {first:?})"
        );
        assert!(
            subscriber_rx.try_recv().is_err(),
            "the second consumer must NOT deliver the same fire again"
        );
        assert!(
            parked_inbound.lock().await.is_empty(),
            "nothing may be parked when the subscriber is live and the event is claimed"
        );
    }

    // ── Memory Transparency Slice M4 (`memory.hygiene`) ────────────────────

    /// Minimal fixture for the sentinel-role fire tests below: a bare hotel
    /// graph with no guests/role incarnations, since a `memory.hygiene` fire
    /// never reaches guest resolution — `fire()` intercepts it before any of
    /// that machinery runs.
    fn memory_hygiene_ticker(
        graph: Arc<GraphDomain>,
    ) -> (
        CronTicker,
        crate::service::ipc::ParkedInboundRegistry,
        tokio::sync::mpsc::UnboundedReceiver<crate::LedgerCommand>,
    ) {
        use std::collections::HashMap;
        use tokio::sync::Mutex;

        let (dispatcher_tx, dispatcher_rx) = crate::service::ipc::test_dispatcher_channel();
        let parked_inbound: crate::service::ipc::ParkedInboundRegistry =
            Arc::new(Mutex::new(HashMap::new()));
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));
        let ticker = CronTicker::new(
            graph,
            dispatcher_tx,
            inboxes,
            "local-aiua-01",
            0,
            parked_inbound.clone(),
            None,
            crate::service::ipc::new_delivery_claim_registry(),
        );
        (ticker, parked_inbound, dispatcher_rx)
    }

    fn memory_hygiene_job(hotel_name: &str, next_fire_at: u64) -> CronJob {
        CronJob {
            id: crate::memory_hygiene::cron_job_id(hotel_name),
            schedule: crate::memory_hygiene::DEFAULT_SCHEDULE.to_string(),
            target_role: crate::memory_hygiene::CRON_TARGET_ROLE.to_string(),
            target_node_id: None,
            payload: "{}".into(),
            guaranteed: false,
            enabled: true,
            last_fired_epoch: None,
            next_fire_at,
            created_at: 0,
            created_by: CronJobSource::Operator,
            silent_ok: true,
            session_target: ansible_mesh_core::cron::CronSessionTarget::Isolated,
        }
    }

    /// Regression for the mesh-replication leak: `CronJobSync` replicates a
    /// `CronJob` *definition* to every peer hotel unconditionally
    /// (`handle_cron_job_sync` upserts without checking any local flag), so a
    /// `memory.hygiene` job an operator enabled on one hotel becomes locally
    /// due on every mesh-connected peer too. A peer that never set
    /// `PHILOTIC_MEMORY_HYGIENE_ENABLED` must NOT run the sweep just because
    /// the job record exists locally — `with_memory_hygiene`'s
    /// `enabled_locally` flag (captured from this hotel's own env at boot)
    /// is what actually enforces "operator opts in per hotel".
    #[tokio::test]
    async fn memory_hygiene_fire_skips_when_not_locally_enabled() {
        use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;

        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let (ticker, parked_inbound, mut dispatcher_rx) = memory_hygiene_ticker(graph.clone());
        // Context wired but NOT locally enabled — the state a peer hotel is
        // in after CronJobSync replicates a job it never opted into.
        let ticker = ticker.with_memory_hygiene(None, "local-hotel", None, false, None);

        let job = memory_hygiene_job("local-hotel", 1_000);
        graph.upsert_cron_job(&job).expect("seed job");

        tokio::time::timeout(Duration::from_secs(2), ticker.fire(&job, 1_000))
            .await
            .expect("fire() must not hang when the sweep is skipped");

        // The schedule still advances — a skipped sweep must not wedge the
        // cron job into permanently re-firing at the same due time.
        let after = graph
            .get_cron_job(&job.id)
            .expect("lookup")
            .expect("job still present");
        assert!(
            after.next_fire_at > 1_000,
            "schedule must advance even when the sweep is locally disabled"
        );

        // No guest delivery, no parked task, no ledger append — the sentinel
        // role never reaches any of that machinery.
        assert!(parked_inbound.lock().await.is_empty());
        assert!(dispatcher_rx.try_recv().is_err());

        // No autonomy grant/audit — proof the sweep body was never entered.
        assert!(
            graph
                .get_autonomy_grant(ansible_mesh_core::autonomy::LANE_MEMORY_HYGIENE)
                .expect("lookup")
                .is_none()
        );
        assert!(
            crate::memory_hygiene::get_last_sweep_run(&graph, "local-hotel")
                .expect("lookup")
                .is_none(),
            "no sweep ran, so no last-run marker should exist"
        );
    }

    /// Companion to the skip test: with `enabled_locally = true` but no
    /// Muninn config wired, the sweep is entered (unlike the skip case) and
    /// short-circuits inside `run_scheduled_sweep` instead — still no panic,
    /// still advances the schedule, still no guest-delivery side effects.
    #[tokio::test]
    async fn memory_hygiene_fire_enters_sweep_when_locally_enabled() {
        use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;

        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let (ticker, parked_inbound, mut dispatcher_rx) = memory_hygiene_ticker(graph.clone());
        let ticker = ticker.with_memory_hygiene(None, "local-hotel", None, true, None);

        let job = memory_hygiene_job("local-hotel", 1_000);
        graph.upsert_cron_job(&job).expect("seed job");

        tokio::time::timeout(Duration::from_secs(2), ticker.fire(&job, 1_000))
            .await
            .expect("fire() must not hang");

        let after = graph
            .get_cron_job(&job.id)
            .expect("lookup")
            .expect("job still present");
        assert!(after.next_fire_at > 1_000);
        assert!(parked_inbound.lock().await.is_empty());
        assert!(dispatcher_rx.try_recv().is_err());
    }

    /// `fire()` must intercept the sentinel role before it ever consults
    /// `resolve_target_role_record` / guest delivery — proven independent of
    /// enablement by checking the sentinel string itself never parses as a
    /// `role:{agent}:{role}` routing key.
    #[test]
    fn memory_hygiene_target_role_is_not_a_role_routing_key() {
        assert!(
            crate::memory_hygiene::CRON_TARGET_ROLE
                .strip_prefix("role:")
                .is_none(),
            "the sentinel must never be mistaken for a role incarnation routing key"
        );
    }

    // ── Autopoiesis Slice A4 (`aria-architect-charter`) ─────────────────────

    fn architect_charter_job(hotel_name: &str, target_role: &str, next_fire_at: u64) -> CronJob {
        CronJob {
            id: crate::architect_charter::cron_job_id(hotel_name),
            schedule: crate::architect_charter::DEFAULT_SCHEDULE.to_string(),
            target_role: target_role.to_string(),
            target_node_id: None,
            payload: r#"{"message":"Run your daily architect-charter sweep now."}"#.into(),
            guaranteed: false,
            enabled: true,
            last_fired_epoch: None,
            next_fire_at,
            created_at: 0,
            created_by: CronJobSource::Operator,
            silent_ok: false,
            session_target: ansible_mesh_core::cron::CronSessionTarget::Isolated,
        }
    }

    /// Same mesh-replication-leak concern as `memory_hygiene`, but gated by
    /// job id rather than target_role (see `architect_charter`'s "Fire-time
    /// re-check" module docs): a peer hotel that never opted in must not
    /// deliver to a role that only exists there because a job definition
    /// replicated in.
    #[tokio::test]
    async fn architect_charter_fire_skips_when_not_locally_enabled() {
        use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
        use std::collections::HashMap;
        use tokio::sync::Mutex;

        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let (dispatcher_tx, mut dispatcher_rx) = crate::service::ipc::test_dispatcher_channel();
        let parked_inbound: crate::service::ipc::ParkedInboundRegistry =
            Arc::new(Mutex::new(HashMap::new()));
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));
        let ticker = CronTicker::new(
            graph.clone(),
            dispatcher_tx,
            inboxes,
            "local-aiua-01",
            0,
            parked_inbound.clone(),
            None,
            crate::service::ipc::new_delivery_claim_registry(),
        )
        .with_architect_charter("local-hotel", false);

        let job = architect_charter_job("local-hotel", "role:agent-test:architect", 1_000);
        graph.upsert_cron_job(&job).expect("seed job");

        tokio::time::timeout(Duration::from_secs(2), ticker.fire(&job, 1_000))
            .await
            .expect("fire() must not hang when the charter is skipped");

        let after = graph
            .get_cron_job(&job.id)
            .expect("lookup")
            .expect("job still present");
        assert!(
            after.next_fire_at > 1_000,
            "schedule must advance even when the charter is locally disabled"
        );
        assert!(parked_inbound.lock().await.is_empty());
        assert!(dispatcher_rx.try_recv().is_err());
    }

    /// This hotel opted in for its OWN job id, but the fired job belongs to a
    /// different hotel (a mesh-replicated peer registration) — must still be
    /// skipped, unlike `memory.hygiene`'s target-role-only gate, because
    /// delivering here could target a role that means something different
    /// (or nothing) on this hotel.
    #[tokio::test]
    async fn architect_charter_fire_skips_replicated_peer_job_even_when_enabled_locally() {
        use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
        use std::collections::HashMap;
        use tokio::sync::Mutex;

        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let (dispatcher_tx, mut dispatcher_rx) = crate::service::ipc::test_dispatcher_channel();
        let parked_inbound: crate::service::ipc::ParkedInboundRegistry =
            Arc::new(Mutex::new(HashMap::new()));
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));
        let ticker = CronTicker::new(
            graph.clone(),
            dispatcher_tx,
            inboxes,
            "local-aiua-01",
            0,
            parked_inbound.clone(),
            None,
            crate::service::ipc::new_delivery_claim_registry(),
        )
        // Enabled locally — but for "local-hotel", not "peer-hotel".
        .with_architect_charter("local-hotel", true);

        let job = architect_charter_job("peer-hotel", "role:agent-test:architect", 1_000);
        graph.upsert_cron_job(&job).expect("seed replicated job");

        tokio::time::timeout(Duration::from_secs(2), ticker.fire(&job, 1_000))
            .await
            .expect("fire() must not hang");

        assert!(parked_inbound.lock().await.is_empty());
        assert!(dispatcher_rx.try_recv().is_err());
    }

    /// The positive path: this hotel's own job, locally enabled — `fire()`
    /// falls through to the normal role-delivery path (PR #80 heritage,
    /// unmodified) exactly like any other role-targeted cron job.
    #[tokio::test]
    async fn architect_charter_fire_delivers_normally_when_enabled_and_matching_id() {
        use ansible_mesh_core::graph::{RoleReadinessState, TurnLoopConfig};
        use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
        use std::collections::HashMap;
        use tokio::sync::Mutex;

        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-test".into(),
                role_name: "architect".into(),
                guest_id: "agent-test:architect".into(),
                toolset_profile: "architect".into(),
                readiness_state: RoleReadinessState::Configured,
                turn_loop_config: TurnLoopConfig::default(),
                ..Default::default()
            })
            .expect("seed role incarnation");

        let (dispatcher_tx, mut dispatcher_rx) = crate::service::ipc::test_dispatcher_channel();
        let parked_inbound: crate::service::ipc::ParkedInboundRegistry =
            Arc::new(Mutex::new(HashMap::new()));
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));
        let ticker = CronTicker::new(
            graph.clone(),
            dispatcher_tx,
            inboxes,
            "local-aiua-01",
            0,
            parked_inbound.clone(),
            None,
            crate::service::ipc::new_delivery_claim_registry(),
        )
        .with_architect_charter("local-hotel", true);

        let job = architect_charter_job("local-hotel", "role:agent-test:architect", 1_000);
        graph.upsert_cron_job(&job).expect("seed job");

        tokio::time::timeout(Duration::from_secs(2), ticker.fire(&job, 1_000))
            .await
            .expect("fire() must not hang");

        // No live subscriber and no materialization_requester wired in this
        // fixture, so the task is parked rather than delivered directly —
        // proof the normal role-delivery path was actually entered (unlike
        // the skip tests above, which never append/park anything).
        let appended = tokio::time::timeout(Duration::from_secs(2), dispatcher_rx.recv())
            .await
            .expect("dispatcher channel should yield the appended command")
            .expect("dispatcher channel should stay open");
        let crate::LedgerCommand::AppendLocal(envelope) = appended else {
            panic!("fire() should append the TaskInvoke via AppendLocal");
        };
        assert!(matches!(envelope.kind, EventKind::TaskInvoke));
        assert_eq!(
            envelope.target_agent_id.as_deref(),
            Some("role:agent-test:architect")
        );
    }

    // ── Autopoiesis Slice A9 outcome-stamping follow-up (`autonomy_sweep`) ──

    fn autonomy_sweep_ticker(
        graph: Arc<GraphDomain>,
    ) -> (
        CronTicker,
        crate::service::ipc::ParkedInboundRegistry,
        tokio::sync::mpsc::UnboundedReceiver<crate::LedgerCommand>,
    ) {
        use std::collections::HashMap;
        use tokio::sync::Mutex;

        let (dispatcher_tx, dispatcher_rx) = crate::service::ipc::test_dispatcher_channel();
        let parked_inbound: crate::service::ipc::ParkedInboundRegistry =
            Arc::new(Mutex::new(HashMap::new()));
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));
        let ticker = CronTicker::new(
            graph,
            dispatcher_tx,
            inboxes,
            "local-aiua-01",
            0,
            parked_inbound.clone(),
            None,
            crate::service::ipc::new_delivery_claim_registry(),
        );
        (ticker, parked_inbound, dispatcher_rx)
    }

    fn autonomy_sweep_job(job_id_hotel: &str, next_fire_at: u64) -> CronJob {
        CronJob {
            id: crate::autonomy_sweep::cron_job_id(job_id_hotel),
            schedule: crate::autonomy_sweep::DEFAULT_SCHEDULE.to_string(),
            target_role: crate::autonomy_sweep::CRON_TARGET_ROLE.to_string(),
            target_node_id: None,
            payload: "{}".into(),
            guaranteed: false,
            enabled: true,
            last_fired_epoch: None,
            next_fire_at,
            created_at: 0,
            created_by: CronJobSource::Operator,
            silent_ok: true,
            session_target: ansible_mesh_core::cron::CronSessionTarget::Isolated,
        }
    }

    fn seed_old_pending_audit(graph: &GraphDomain, created_at: u64) {
        let record = ansible_mesh_core::autonomy::AutonomyAuditRecord::new(
            "old-pending",
            ansible_mesh_core::autonomy::AutonomyLane::new(
                ansible_mesh_core::autonomy::LANE_GRAPH_BRIDGE_EDGES,
            ),
            "did a thing",
            "evidence",
            "revert the thing",
            ansible_mesh_core::autonomy::AutonomyPosture::ConfirmFirst,
            created_at,
        );
        graph.record_autonomy_audit(&record).expect("seed audit");
    }

    const EIGHT_DAYS_SECS: u64 = 8 * 86_400;

    /// The load-bearing mesh-trap regression: `CronJobSync` replicates a
    /// `CronJob` *definition* to every peer hotel unconditionally, so
    /// `hotel-a`'s `autonomy-outcome-sweep:hotel-a` job can become locally
    /// due on `hotel-b` too. Because this sweep has no `enabled_locally`
    /// opt-in (unlike memory.hygiene/dream-sweep), the only thing preventing
    /// `hotel-b` from sweeping (and mis-attributing) `hotel-a`'s audit
    /// records is the job-id match in `fire_autonomy_sweep`. Behavioral
    /// proof, not a marker check: seed an aged Pending audit, fire the
    /// mismatched job on `hotel-b`'s ticker, assert the record is still
    /// Pending.
    #[tokio::test]
    async fn autonomy_sweep_fire_skips_a_replicated_peer_hotel_job() {
        use ansible_mesh_core::autonomy::AuditOutcome;
        use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;

        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        // now_ms/1000 - EIGHT_DAYS_SECS puts the record 8 days before "now".
        let now_ms: u64 = (EIGHT_DAYS_SECS + 1_000) * 1000;
        seed_old_pending_audit(&graph, 1_000);

        let (ticker, parked_inbound, mut dispatcher_rx) = autonomy_sweep_ticker(graph.clone());
        // This ticker believes it is hotel-b.
        let ticker = ticker.with_autonomy_sweep("hotel-b");

        // But the fired job carries hotel-a's deterministic id — exactly
        // what a CronJobSync replication of hotel-a's own registration
        // looks like once it lands in hotel-b's local cron_jobs table.
        let job = autonomy_sweep_job("hotel-a", 1_000);
        graph.upsert_cron_job(&job).expect("seed job");

        tokio::time::timeout(Duration::from_secs(2), ticker.fire(&job, now_ms))
            .await
            .expect("fire() must not hang when the sweep is skipped");

        // Schedule still advances — a skipped sweep must not wedge the job
        // into permanently re-firing at the same due time.
        let after = graph
            .get_cron_job(&job.id)
            .expect("lookup")
            .expect("job still present");
        assert!(after.next_fire_at > 1_000);

        // The load-bearing assertion: the aged Pending record must be
        // UNTOUCHED — hotel-b never entered the sweep body for hotel-a's job.
        let record = graph
            .get_autonomy_audit("old-pending")
            .expect("lookup")
            .expect("record exists");
        assert_eq!(
            record.outcome,
            AuditOutcome::Pending,
            "a mismatched job id must never stamp another hotel's audit records"
        );

        assert!(parked_inbound.lock().await.is_empty());
        assert!(dispatcher_rx.try_recv().is_err());
    }

    /// Companion to the skip test: when the fired job's id matches this
    /// hotel's own deterministic job id, the sweep runs and stamps the aged
    /// Pending record Neutral.
    #[tokio::test]
    async fn autonomy_sweep_fire_runs_for_this_hotels_own_job() {
        use ansible_mesh_core::autonomy::AuditOutcome;
        use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;

        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let now_ms: u64 = (EIGHT_DAYS_SECS + 1_000) * 1000;
        seed_old_pending_audit(&graph, 1_000);

        let (ticker, parked_inbound, mut dispatcher_rx) = autonomy_sweep_ticker(graph.clone());
        let ticker = ticker.with_autonomy_sweep("hotel-a");

        let job = autonomy_sweep_job("hotel-a", 1_000);
        graph.upsert_cron_job(&job).expect("seed job");

        tokio::time::timeout(Duration::from_secs(2), ticker.fire(&job, now_ms))
            .await
            .expect("fire() must not hang");

        let record = graph
            .get_autonomy_audit("old-pending")
            .expect("lookup")
            .expect("record exists");
        assert_eq!(
            record.outcome,
            AuditOutcome::Neutral,
            "this hotel's own job id must run the sweep and stamp the aged Pending record"
        );

        assert!(parked_inbound.lock().await.is_empty());
        assert!(dispatcher_rx.try_recv().is_err());
    }

    /// Same independence-from-role-routing proof as
    /// `memory_hygiene_target_role_is_not_a_role_routing_key`.
    #[test]
    fn autonomy_sweep_target_role_is_not_a_role_routing_key() {
        assert!(
            crate::autonomy_sweep::CRON_TARGET_ROLE
                .strip_prefix("role:")
                .is_none(),
            "the sentinel must never be mistaken for a role incarnation routing key"
        );
    }
}
