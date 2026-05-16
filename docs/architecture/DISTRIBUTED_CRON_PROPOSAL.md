---
title: Distributed Cron Scheduler Proposal
doc_type: proposal
domain: runtime-and-sessions
status: accepted-current-slice
last_updated: 2026-03-31
tags:
- cron
- scheduling
- hotel
- mesh
- guaranteed-delivery
- aiua
related_docs:
- ARCHITECTURE_STATUS.md
- RUNTIME_AUTHORITY_LEASES_PROPOSAL.md
- TASK_RUNNER_PROPOSAL.md
- INTER_HOTEL_ROUTING_PROPOSAL.md
- TELEGRAM_POLL_LEASE_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: distributed-cron-scheduler
implements:
- hotel-scheduler
active_seams: []
---

# Distributed Cron Scheduler Proposal

## Goal

Give the hotel daemon (aiua) a first-class cron subsystem. Scheduled work is expressed as **pre-packaged envelopes** authored at registration time, stored in the hotel's SQLite graph, and delivered via the existing event ledger + inbox dispatch path. Guaranteed jobs achieve at-least-once delivery across the mesh via staggered-offset deduplication.

---

## Core Recommendation

**Cron entries are graph-stored `CronJob` records.** When a job fires, the hotel materializes a full `EventEnvelope` (kind `TaskInvoke`) and writes it to `EventStorage`. The existing 1-second mesh dispatcher delivers it — no special delivery path needed.

Two delivery modes:

| Mode | Behavior |
|---|---|
| `guaranteed = false` | Local-only, fire-or-skip. No mesh coordination. Default for idempotent/best-effort tasks. |
| `guaranteed = true` | Mesh-coordinated. Staggered per-hotel offset. At-least-once delivery. Deduplication via `CronFired` broadcast. |

---

## Data Model

### `CronJob`

```rust
struct CronJob {
    id: CronJobId,                      // UUID
    schedule: String,                   // cron expression ("0 */5 * * * *")
    target_role: String,                // inbox role to deliver to
    target_node_id: Option<NodeId>,     // None = local hotel; Some = remote (Slice 3+)
    payload: String,                    // static JSON; `{timestamp}` always interpolated
    guaranteed: bool,
    enabled: bool,
    last_fired_epoch: Option<u64>,      // ms — intended fire time (not wall clock)
    next_fire_at: u64,                  // ms — absolute next intended fire time
    created_at: u64,
    created_by: CronJobSource,          // Operator | Guest(GuestId)
}

enum CronJobSource {
    Operator,
    Guest(GuestId),
}
```

### `{timestamp}` interpolation

Every payload — guaranteed or not — has `{timestamp}` replaced with the `fire_epoch` (ms since Unix epoch) at fire time. This is the only built-in template variable in Slice 1. Additional interpolation is a later slice.

---

## Distributed Guaranteed Protocol

### Hotel offset assignment

Each hotel is assigned a `cron_offset_secs: u64` in `mesh-config.json`. Convention: assign offsets in 5-second increments by hotel priority (primary = 0, secondary = 5, tertiary = 10, …). Ties resolved by `NodeId` lexicographic order.

```json
// mesh-config.json (new field)
{
  "cron_offset_secs": 0
}
```

### Fire sequence for `guaranteed = true`

```
fire_epoch = next_fire_at (the scheduled instant — dedup key)
effective_fire_at = fire_epoch + cron_offset_secs * 1000

At effective_fire_at:
  1. Query EventStorage: any CronFired { job_id, fire_epoch } already in ledger?
  2. If yes → suppress (another hotel already fired this epoch)
  3. If no  → materialize EventEnvelope, append to EventStorage
           → broadcast CronFired { job_id, fire_epoch, fired_by: self.node_id }
           → update last_fired_epoch = fire_epoch
           → advance next_fire_at
```

### Recovery / missed-fire (guaranteed only)

On hotel startup, for every enabled guaranteed job where `next_fire_at < now`:
1. Check EventStorage for `CronFired { job_id, fire_epoch: next_fire_at }`.
2. If found → job was already handled by another hotel; advance `next_fire_at` and continue.
3. If not found → fire immediately (all hotels may have been down); then advance.

Non-guaranteed jobs: missed intervals are silently skipped. `next_fire_at` is advanced without firing.

---

## New Event Kinds

```rust
// added to EventKind enum
CronFired {
    job_id: CronJobId,
    fire_epoch: u64,        // the intended next_fire_at that was fired
    fired_by: NodeId,
},
CronJobSync {
    job: CronJob,           // full job record for mesh propagation (Slice 3)
},
```

`CronFired` events are written as normal `EventEnvelope` entries — durable, ledger-ordered, queryable by the dispatcher.

---

## Storage Trait

```rust
// new trait in ansible-mesh-core
trait CronStorage: Send + Sync {
    fn upsert_job(&self, job: &CronJob) -> Result<()>;
    fn remove_job(&self, id: CronJobId) -> Result<()>;
    fn get_job(&self, id: CronJobId) -> Result<Option<CronJob>>;
    fn list_jobs(&self) -> Result<Vec<CronJob>>;
    // Returns jobs whose effective_fire_at (fire_epoch + offset) <= now
    fn list_due(&self, now_ms: u64, offset_ms: u64) -> Result<Vec<CronJob>>;
    fn mark_fired(&self, id: CronJobId, fire_epoch: u64) -> Result<()>;
    fn advance_schedule(&self, id: CronJobId, next_fire_at: u64) -> Result<()>;
}
```

SQLite implementation lives in `ansible-mesh-core/src/storage/sqlite_cron_storage.rs`, consistent with existing storage trait pattern.

---

## IPC Surface

New `IpcRequest` variants (added to `philotic-client/src/lib.rs`):

```rust
RegisterCronJob { job: CronJobSpec },       // guest or operator registers/updates a job
RemoveCronJob { job_id: CronJobId },
ListCronJobs,                               // returns all jobs on this hotel
EnableCronJob { job_id: CronJobId },
DisableCronJob { job_id: CronJobId },
```

New `IpcResponse` variant:
```rust
CronJobList { jobs: Vec<CronJob> },
```

`CronJobSpec` is the subset of `CronJob` the caller provides — `id`, `created_by`, `last_fired_epoch`, `next_fire_at`, `created_at` are hotel-assigned.

---

## Hotel Service: `CronTicker`

Runs as an `Arc`-shared service alongside `LeaseProvider` and `InboxRegistry` in `aiua`.

```
loop (1-second tick or earlier on next_fire_at deadline):
  due_jobs = cron_storage.list_due(now, hotel_offset_ms)
  for job in due_jobs:
    if job.guaranteed:
      if cron_fired_exists(job.id, job.next_fire_at):  // check EventStorage
        advance_schedule(job)
        continue
    envelope = build_envelope(job)
    event_storage.append_local(envelope)
    if job.guaranteed:
      fired_event = build_cron_fired(job.id, job.next_fire_at)
      event_storage.append_local(fired_event)
    cron_storage.mark_fired(job.id, job.next_fire_at)
    cron_storage.advance_schedule(job.id, next_from_schedule(job.schedule, now))
```

---

## Disposition

**Accepted for current slice.** Slices 1 and 2 implemented on `codex/cron-scheduler`.

---

## Slice Plan

| Slice | Scope | Branch |
|---|---|---|
| **1** | `CronStorage` trait + SQLite impl, `CronTicker` (local, non-guaranteed), `IpcRequest` register/remove/list, `{timestamp}` interpolation | `codex/cron-scheduler` |
| **2** | `guaranteed` flag, `CronFired` EventKind, staggered-offset dedup, recovery scan at startup | `codex/cron-scheduler` |
| **3** | `CronJobSync` — propagate job definitions across mesh at boot/change | future |
| **4** | Additional template variables beyond `{timestamp}` | future |

**Current Slice: 3**

Active seam: none yet — seam entry added when `CronTicker` is wired into aiua main.

---

## Open Questions

- Should `cron_offset_secs` be auto-assigned from `NodeId` hash, or always explicit in mesh-config? (Explicit is safer for predictable priority ordering.)
- Should `CronJobSync` replicate jobs to all mesh peers at registration, or only on demand? (Demand is simpler; full sync on connect is more robust for recovery.)
- Should operator-registered jobs survive hotel restarts without re-registration? (Yes — stored in SQLite, always reloaded on boot. Guest-registered jobs must re-register on guest startup.)
