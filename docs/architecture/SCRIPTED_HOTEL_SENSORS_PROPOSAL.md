---
title: Scripted Hotel Sensors Proposal
doc_type: proposal
domain: runtime-and-sessions
status: proposed
last_updated: 2026-08-31
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
correct and stays; only its home changes).

**Pivot, 2026-08-31**: the first design (below the line, for history) put
the Rhai engine in `aiua`'s `CronTicker`, reusing the sentinel `target_role`
intercept the Distributed Cron Scheduler
([DISTRIBUTED_CRON_PROPOSAL.md](DISTRIBUTED_CRON_PROPOSAL.md)) already uses
for `memory.hygiene`, dream-sweep, and autonomy-sweep. Tracing the mesh/tool
dispatch code (`crates/philote/src/tool_exec.rs`, `turn_loop.rs`,
`crates/datasource/src/runtime.rs`) to design `query_remote` surfaced a
better fit: `data-memorygraphrag` already runs a generic inbound-task loop
(`run_datasource_controller`) that resolves a `DatasourceProvider` by task
kind and calls `invoke()` — exactly what `LifeGraphProvider` already does
for the 18 `life.*` tools. A sensor is now a `SensorProvider` living in that
*same* process, implementing that *same* trait, holding an `Arc` to the
guest's own `LifeGraphProvider` — so `life_call` binds directly to
`LifeGraphProvider::invoke()` with zero IPC hop, and the mesh-LifeGraph
question (the proposal's biggest open question below) disappears rather
than needing a new proxy. `aiua`'s `CronTicker` needed **no new code at
all**: a sensor `CronJob` is an ordinary `target_role: "life-graph-runner"`
delivery, identical to any other cron job. The slice-1 sentinel intercept
and in-process engine that had briefly lived in `crates/aiua/src/
sensor_scripts.rs` were removed in the same commit as this rewrite.

## Core Recommendation

**One new `DatasourceProvider`, N hotel-config-stored scripts.**
`data-memorygraphrag` gains `SensorProvider` (`crates/data-memorygraphrag/
src/sensor_provider.rs`), registered alongside `LifeGraphProvider` in the
guest's provider list. It matches `DatasourceTask.kind == "sensor.run"`.
The task's `parameters.sensor_id` names the sensor; the check logic is a
**Rhai script** (`crates/data-memorygraphrag/src/sensor_scripts.rs`) stored
as ordinary hotel config (`sensor_script:<sensor_id>`, same storage
`heartbeat_chat_id` already uses — durable, no ansible, active on the very
next tick, read/written over the guest's existing `GetConfig`/`SetConfig`
IPC).

```
CronJob { target_role: "life-graph-runner",
          payload: {"kind": "sensor.run", "parameters": {"sensor_id": "reminders"}},
          schedule: "0 */5 * * * * *" }
        │
        ▼ ordinary cron→role delivery — the ONE thing fire() needed to learn:
          a DatasourceTask-shaped payload (top-level "kind") is passed
          through unwrapped, not run through the philote-turn-shaped
          build_cron_task_json (which nests payload as a string and would
          hide "kind"/"parameters" from DatasourceTask::from_value —
          caught by review before any live traffic hit it; regression test:
          datasource_shaped_cron_payload_survives_fire_unwrapped)
data-memorygraphrag's run_datasource_controller receives the task
        │  ProviderRegistry resolves SensorProvider (kind == "sensor.run")
        ▼
SensorProvider::invoke()
        │  loads sensor_script:reminders over GetConfig (guest's real identity)
        │  runs the Rhai engine (sensor_scripts::run_script), binding a
        │  curated function surface (below) — life_call goes straight to
        │  this process's own LifeGraphProvider::invoke(), no IPC
        ▼
Rhai script returns: Quiet | Deliver{target_role, message} | Investigate{target_role, brief}
        │
        ▼ Deliver reuses the SAME EmitTask IPC call PR #463's heartbeat
          prototype already proved live — under the guest's real identity,
          not a synthetic "philotic-heartbeat" one.
```

Why Rhai: pure Rust (`rhai` crate, no C dependency — worth protecting,
vps-jane already OOM-kills release links on a heavier build), sandboxed by
default (no file/process/network access unless a function is explicitly
registered into the engine), and cheap to embed. The operator only ever
writes the check logic; the framework owns everything that can hurt the
hotel. Rhai's native functions are synchronous, but `life_call`'s
underlying work (`LifeGraphProvider::invoke`) is `async` — the bridge is
`tokio::task::block_in_place` + `Handle::block_on`, which requires (and
gets, by default) a multi-threaded Tokio runtime.

## Script API surface (curated, not general-purpose)

Registered native functions — this is the entire capability surface a
script gets, deliberately narrow:

| Function | Purpose | Status |
|---|---|---|
| `config_value(key) -> string` | Read a single hotel config value. | Implemented |
| `now_iso()` | Current UTC time, ISO 8601. | Implemented |
| `operator_local(iso, tz) -> string` | Format a UTC timestamp in an operator timezone. | Implemented |
| `deliver(target_role, message)` | Pure delivery — pre-formatted text, the agent just relays it. | Implemented |
| `investigate(target_role, brief)` | Hand a finding to a philote for actual reasoning, via the paracrine lookaside (`delegate.whisper`, see [WHISPER_PROTOCOL_PROPOSAL.md](WHISPER_PROTOCOL_PROPOSAL.md)). | Verdict wired, dispatch path not yet built |
| `life_call(tool, args) -> map` | Call any `life.*` tool (`life.recall`, `life.list`, `life.view.node`, …) against the mesh LifeGraph, in-process. | Implemented — replaces the `query_remote` design below |

No file IO, no process spawn, no arbitrary network — a script that needs a
capability outside this surface is a signal the framework needs a new
function, not that scripts should get broader access.

## Data model

```rust
// crates/data-memorygraphrag/src/sensor_scripts.rs — hotel-config-backed
// record (sensor_script:<id>), same pattern as config:heartbeat_chat_id,
// read/written by the guest over GetConfig/SetConfig IPC.
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
`target_role` is an ordinary role-delivery target (not a sentinel).
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
  (Not done yet — see Status, slice 2.)
- The `philotic-heartbeat` *synthetic-identity* pseudo-guest pattern in
  `fetch_hotel_config`/`emit_delivery_turn` (`main.rs`) — `SensorProvider`'s
  equivalents (`fetch_config`/`deliver` in `sensor_provider.rs`) still open
  a fresh IPC connection per call (`DatasourceProvider::invoke` has no
  access to the runtime's already-open connection, and changing that trait
  would touch every provider), but now under the guest's *real* identity,
  and the quiet-tick path — the overwhelming common case — never connects
  at all.
- `PHILOTIC_HEARTBEAT_*` env vars — schedule and chat routing become
  ordinary `CronJob` fields, administered the same way every other cron
  job already is.
- The "single-deliverer is structural because the runner materializes on
  one hotel" design note — fragile-by-convention. `CronTicker`'s existing
  `guaranteed`/staggered-offset/`CronFired` dedup already solves
  exactly-once-across-the-mesh properly, for free, if a sensor ever needs
  it.
- The `aiua`-side sentinel intercept and in-process Rhai engine slice 1
  originally built (`crates/aiua/src/sensor_scripts.rs`, the `fire()`
  intercept, the `rhai` dependency in `crates/aiua/Cargo.toml`) — removed
  in the same commit as this pivot. A sensor `CronJob` needs no special
  case in `CronTicker` at all.

## Status

**Framework — done, test-green** (`codex/scripted-hotel-sensors`):
`SensorProvider` registered in `data-memorygraphrag`'s provider list,
`SensorScript` hotel-config storage (over `GetConfig`/`SetConfig` IPC),
`config_value`/`now_iso`/`operator_local`/`deliver`/`life_call` wired end
to end, `life_call` bound directly to the guest's own
`LifeGraphProvider::invoke`. 248 tests green across `data-memorygraphrag`
(195 lib + 53 bin — 6 new `sensor_scripts` tests covering `run_script`
against hand-built closures including the `life_call` round-trip, plus 2
new `sensor_provider` tests exercising `SensorProvider::invoke` itself:
graceful quiet-degrade when the hotel IPC socket is unreachable, and
`contract_error` rejection of a task missing `sensor_id`). A separate
`aiua` regression test
(`datasource_shaped_cron_payload_survives_fire_unwrapped`) locks the wire
contract between `fire()`'s emitted JSON and `DatasourceTask::from_value`.
Not yet covered: the `block_in_place`/`life_call` bridge and `deliver`'s
`EmitTask` path both require a script to actually load, which needs a
live hotel IPC socket — no test exercises them end to end yet; that gap
closes naturally once slice 2 is watched-live-green on a real hotel.
`investigate` returns a verdict but has no dispatch path yet.

**Slice 2 — not started.** Port `heartbeat.rs`'s due-reminder logic
(`due_reminders_cypher`/`stamp_cypher`/`format_reminder_line`) to a Rhai
script driving `life_call`, replacing `heartbeat_reminders_tick`. No
longer blocked on a `query_remote` proxy — `life_call` already reads the
mesh LifeGraph in-process.

**Slice 3 — not started.** Delete the retired `data-memorygraphrag`
heartbeat module/pseudo-guest/env-vars once slice 2 is watched-live-green
on vps-jane. Extend `cron.list`-style tooling to show sensor jobs with
their `last_result`.

## Open Questions

- ~~Sentinel `target_role` intercept vs. a new `job_kind` discriminant
  field on `CronJob`?~~ **Resolved, then superseded**: the sentinel
  pattern worked cleanly in slice 1, but the pivot to `SensorProvider`
  removed the need for any `aiua`-side dispatch special-case at all — a
  sensor `CronJob` is now an ordinary role delivery.
- ~~How should a sensor script read the mesh LifeGraph, given `aiua` has
  no bolt/cypher client and only the `data-memorygraphrag` guest does?~~
  **Resolved by the pivot**: it doesn't need to — the engine now lives
  *in* `data-memorygraphrag`, so `life_call` is an in-process call to
  `LifeGraphProvider::invoke`, not a cross-process proxy.
- Does `operator_approved` on a `SensorScript` need UI/ceremony beyond "an
  operator or an agent with standing approval authority wrote this row"?
  Leaning no for now — matches `heartbeat_chat_id`'s existing trust
  boundary (whoever can write to this hotel's config). Revisit if a script
  surface becomes reachable from a lower-trust caller.
- Should `sensor.run` join `is_read_only_capability`'s whitelist in
  `crates/datasource/src/runtime.rs`? Deliberately left off for now: an
  unlisted kind takes the *write* path (inline, sequential, and the
  runtime's own comment confirms partial writes before a timeout deadline
  stay durable, not rolled back) — the right default for a sensor that
  stamps dispatch state before emitting. Revisit only if sensor volume
  ever needs the read path's off-critical-path concurrency.
