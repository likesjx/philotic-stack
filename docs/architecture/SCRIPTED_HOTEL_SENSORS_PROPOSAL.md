---
title: Scripted Hotel Sensors Proposal
doc_type: proposal
domain: runtime-and-sessions
status: proposed
last_updated: 2026-08-29
tags:
- cron
- scheduling
- hotel
- rhai
- scripting
- skilldag
- sensors
- no-hardcode
related_docs:
- DISTRIBUTED_CRON_PROPOSAL.md
- WHISPER_PROTOCOL_PROPOSAL.md
- DATA_DRIVEN_TOOL_GRANTS_PROPOSAL.md
proposal_id: scripted-hotel-sensors
implements:
- hotel-scheduler
extends:
- distributed-cron-scheduler
active_seams:
- scripted-hotel-sensors
---

# Scripted Hotel Sensors — Conditional Cron via Embedded Rhai Checks

## Goal

Give the hotel a **conditional cron job kind**: a `CronJob` that, on schedule,
runs a small graph-stored script to decide whether there is real work — and
only spends an agent turn if the script says yes. New sensors ship as graph
data, not Rust + a deploy.

## Problem — what today's prototype got right and where it doesn't scale

PR #463 (`crates/data-memorygraphrag/src/heartbeat.rs`) proved the pattern
live on vps-jane 2026-08-28: a deterministic due-reminder check runs every
5 minutes, and a Beacon turn fires only when something is actually due
(watched-live-green — quiet tick and fired tick both confirmed). But it has
three structural problems, all a direct consequence of being hand-written
Rust living in a guest instead of hotel-native scheduling:

1. **One sensor = one Rust PR + CI + `vps-deploy-ci`.** Adding a second
   check (e.g. "stale open loop over 30 days," "unconfirmed proposed fact
   over 7 days") means another cypher-builder module, another `main.rs`
   wire-up, another deploy cycle — exactly the hardcoding pattern the
   operator already flagged as wrong for tool grants
   (see [DATA_DRIVEN_TOOL_GRANTS_PROPOSAL.md](DATA_DRIVEN_TOOL_GRANTS_PROPOSAL.md)).
2. **The guest pretends to be an IPC client of itself.** `spawn_heartbeat_timer`
   connects to the hotel over the Unix socket as a fake guest identity
   (`guest_id: "philotic-heartbeat"`) from *inside* `data-memorygraphrag`,
   just to call `GetConfig`/`EmitTask` — a workaround for not being in the
   hotel process. `service::cron_ticker::fire()` already does this dispatch
   as a first-class in-process call for every other cron job kind.
3. **No administration surface.** An operator has no way to ask "what
   sensors exist, on what interval, when did each last fire, what did it
   decide" — it's implicit in Rust source and environment variables. Cron
   jobs are graph rows with `cron.list`; sensors should be too.

This proposal keeps the debugged core (the due-reminder check itself is
correct and stays; only its home changes) and moves scheduling + execution
into the hotel's existing `CronTicker`, exactly where the Distributed Cron
Scheduler ([DISTRIBUTED_CRON_PROPOSAL.md](DISTRIBUTED_CRON_PROPOSAL.md))
already fires `memory.hygiene`, dream-sweep, and autonomy-sweep in-process
via a sentinel `target_role` intercept in `fire()`.

## Core Recommendation

**One new sentinel, N graph-stored scripts** — not one sentinel per sensor.
`fire()` gains a single new intercept: jobs whose `target_role` is
`crate::sensor_scripts::CRON_TARGET_ROLE`. The job's `payload` names a
`sensor_id`; the actual check logic is a **Rhai script** stored as a graph
node (`config:sensor_script:<sensor_id>`, same `NODE_KIND_CONFIG` storage
`heartbeat_chat_id` already uses — durable, no ansible, active on the very
next tick).

```
CronJob { target_role: sensor_scripts::CRON_TARGET_ROLE,
          payload: {"sensor_id": "reminders"}, schedule: "0 */5 * * * * *" }
        │
        ▼ fire() intercepts
sensor_scripts::evaluate(sensor_id)
        │  loads config:sensor_script:reminders (Rhai source)
        │  binds a curated function surface (below)
        ▼
Rhai script returns: Quiet | Deliver{target_role, message} | Investigate{target_role, brief}
        │
        ▼ Deliver/Investigate fall through to the SAME TaskInvoke / paracrine
          dispatch paths fire() and delegate.whisper already use — no new
          delivery machinery.
```

Why Rhai: pure Rust (`rhai` crate, no C dependency — worth protecting,
vps-jane already OOM-kills release links on a heavier build), sandboxed by
default (no file/process/network access unless a function is explicitly
registered into the engine), and cheap to embed. The operator only ever
writes the check logic; the framework owns everything that can hurt the
hotel.

## Script API surface (curated, not general-purpose)

Registered native functions — this is the entire capability surface a
script gets, deliberately narrow:

| Function | Purpose | Status |
|---|---|---|
| `config_value(key) -> string` | Read a single hotel config value. | Implemented (slice 1) |
| `now_iso()` | Current UTC time, ISO 8601. | Implemented (slice 1) |
| `operator_local(iso, tz) -> string` | Format a UTC timestamp in an operator timezone. | Implemented (slice 1) |
| `deliver(target_role, message)` | Pure delivery — pre-formatted text, the agent just relays it. | Implemented (slice 1) |
| `investigate(target_role, brief)` | Hand a finding to a philote for actual reasoning, via the paracrine lookaside (`delegate.whisper`, see [WHISPER_PROTOCOL_PROPOSAL.md](WHISPER_PROTOCOL_PROPOSAL.md)). | Reserved — errors until wired |
| `query_remote(cypher) -> [map]` | Read the mesh LifeGraph (Memgraph/bolt). | Not started — see Open Questions |

No file IO, no process spawn, no arbitrary network — a script that needs a
capability outside this surface is a signal the framework needs a new
function, not that scripts should get broader access.

## Data model

```rust
// crates/aiua/src/sensor_scripts.rs — NODE_KIND_CONFIG-backed record, not a
// new table, same pattern as config:heartbeat_chat_id.
struct SensorScript {
    id: String,              // "reminders", "stale-open-loops", ...
    source: String,          // Rhai source
    enabled: bool,
    operator_approved: bool, // governance gate — mirrors life.patch.apply's
                              // operator_approved:true requirement; a script
                              // is graph-editable, so it needs the same
                              // "someone with authority signed off" gate
                              // proposed writes to LifeGraph already have.
    last_run_at: Option<u64>,
    last_result: Option<String>,  // "quiet" | "delivered" | "investigate" | "error: ..."
}
```

No new `CronJob` fields — `payload` already carries arbitrary JSON,
`target_role` already selects an in-process intercept for other job kinds.
Intentionally the smallest possible extension of an already-implemented,
already-mesh-aware system. A hotel with no matching `sensor_script:<id>`
row simply has nothing to run when a `CronJobSync`-replicated job
definition arrives from a peer — script presence is the per-hotel gate,
no separate enabled-locally flag needed (unlike `memory.hygiene`/
`dream-sweep`, which do need one).

## What this retires

- `data-memorygraphrag::heartbeat` module and `spawn_heartbeat_timer` —
  the due-reminder check becomes the first `SensorScript`, translated
  from Rust cypher-builders (already just string templates) into Rhai.
- The `philotic-heartbeat` pseudo-guest IPC self-connect pattern
  (`fetch_hotel_config`/`emit_delivery_turn` in `main.rs`).
- `PHILOTIC_HEARTBEAT_*` env vars — schedule and chat routing become
  ordinary `CronJob` fields, administered the same way every other cron
  job already is.
- The "single-deliverer is structural because the runner materializes on
  one hotel" design note — fragile-by-convention. `CronTicker`'s existing
  `guaranteed`/staggered-offset/`CronFired` dedup already solves
  exactly-once-across-the-mesh properly, for free, if a sensor ever needs
  it.

## Status

**Slice 1 — done, test-green** (`codex/scripted-hotel-sensors`, commit
`98e03e3d`): `fire()` sentinel intercept, `SensorScript` graph storage,
`config_value`/`now_iso`/`operator_local`/`deliver` wired end to end.
`investigate` registered but errors (not yet wired to a dispatch path).
`query_remote` deliberately not registered this slice.

**Slice 2 — not started.** Port `heartbeat.rs`'s due-reminder logic to a
Rhai script; requires resolving the `query_remote` open question first,
since the reminders check reads the mesh LifeGraph (Memgraph), which
`aiua` has no client for today.

**Slice 3 — not started.** Delete the retired `data-memorygraphrag`
module/pseudo-guest/env-vars once slice 2 is watched-live-green on
vps-jane. Extend `cron.list`-style tooling to show sensor jobs with their
`last_result`.

## Open Questions

- ~~Sentinel `target_role` intercept vs. a new `job_kind` discriminant
  field on `CronJob`?~~ **Resolved by slice 1**: the sentinel pattern
  works cleanly and is now the 4th job type using it
  (`memory_hygiene`, `dream_sweep`, `autonomy_sweep`, `sensor_scripts`).
  No reason to introduce a schema change for one dispatch point.
- **How should a sensor script read the mesh LifeGraph, given `aiua` has
  no bolt/cypher client and only the `data-memorygraphrag` guest does?**
  This is bigger than the original "graceful degrade" framing — it's an
  unbuilt IPC proxy. Candidate shapes:
  - (a) A new synchronous-shaped `IpcRequest`/`IpcResponse` pair the
    ticker sends to the guest and awaits with a timeout inside `fire()`
    (blocking one cron tick on a guest round-trip — acceptable if bounded
    and rare, since sensors already tolerate a 5-minute cadence).
  - (b) `aiua` grows its own thin bolt client, duplicating what the guest
    already has — more code, but no cross-process hop per tick.
  - (c) Sensors that need remote data register on `data-memorygraphrag`'s
    own tick loop instead of the hotel's, splitting the framework across
    two processes — closest to today's shape, but reintroduces problem 2
    above for exactly the sensors that need it most.
  Lean (a): smallest new surface, keeps the "one hotel-native framework"
  property, and the existing `EmitTask`/`GetConfig` IPC pattern already
  proves guest round-trips from the hotel are workable.
- Does `operator_approved` on a `SensorScript` need UI/ceremony beyond "an
  operator or an agent with standing approval authority wrote this row"?
  Leaning no for now — matches `heartbeat_chat_id`'s existing trust
  boundary (whoever can write to this hotel's graph). Revisit if a script
  surface becomes reachable from a lower-trust caller.
