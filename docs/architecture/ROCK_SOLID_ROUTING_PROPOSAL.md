---
domain: routing-transport
status: proposed
disposition: proposed
last_updated: 2026-07-01
---

# Rock-Solid Routing Proposal — Recoverable Envelope Delivery

**Status**: Proposed
**Domain**: routing-transport (aiua IPC dispatch + mesh)
**Date**: 2026-07-01
**Motivation**: Tool calls and model calls that get stuck create stuck sessions, and
philotes cannot get unstuck on their own. Today recovery is *reactive and coarse* —
the philote turn watchdog is the only backstop, it is philote-scoped, and it takes
90–600 s to fire. The routing layer itself has no per-envelope deadline and silently
drops undeliverable envelopes, so the calling component is never told a route failed.

---

## Goal

Make routing **recoverable by construction**: every routing envelope has a bounded
lifetime, and every terminal outcome (delivered, undeliverable, timed-out,
materialization-failed) produces a signal back to the calling component. A component
that dispatches a tool call or model call must always receive *either* a real
response *or* a typed failure — never silence. This must hold **within a hotel**
(IPC/UDS) and **across hotels** (mesh/UDP), with explicit handling of the nuances the
mesh introduces (best-effort, no ack, remote materialization).

---

## Current State — Why the Existing Backstops Are Insufficient

There are already several recovery mechanisms. The design must extend them, not
duplicate them. Each covers a real case but leaves the goal unmet:

| Mechanism | Layer | What it covers | Why it is insufficient for the goal |
|---|---|---|---|
| `evict_timed_out_turns` (philote runtime) | Philote session | Turns stuck in a waiting phase (WaitingModel 300 s, WaitingTool 90 s, catch-all 600 s) | **Reactive & coarse**: waits 90–600 s; **philote-scoped**: only unsticks a philote that has an active turn — a model-controller, cron emitter, or cross-hotel emitter waiting on a route gets nothing; **recovery quality**: it evicts the turn but does not fix the route, so the next turn re-sticks. |
| `codex/stuck-turn-watchdog` (heal-dispatcher `RepairStaleSessionTurns`, startup WaitingTool eviction) | Hotel sweep | Proactive 30 s zombie-turn scan; evicts stale `running` turns on restart | Same reactive/coarse shape at the hotel level — it *cleans up* stuck turns; it does not prevent a route failure from stranding a caller in the first place. Complementary, not a substitute. |
| `capability_invoke` (aiua IPC) | Synchronous IPC | Model-controller capability calls: pending-oneshot + 60 s timeout + typed error back | Correct pattern, but **only for the synchronous capability path**. Async task delivery (`deliver_inbound_task`) has no equivalent. |
| Golgi `golgi_pipeline_watchdog` | Capability pipeline | Multi-stage capability pipelines: `pending_pipelines` + 120 s TTL sweep + `on_failure` passthrough | Correct pattern, but **scoped to Golgi pipelines**. |
| `deliver_inbound_task` no-subscriber branch | IPC dispatch chokepoint | — | **The silent-drop black hole.** Returns `()`; on no subscriber it only `warn!`s "stays ledger-only for now" and returns. The caller is never told. This is the primary origin of stuck sessions. Mirror sites: cross-hotel drop (`ipc.rs:6966`), paracrine target lookup (`ipc.rs:11868/11875`). |

**Diagnosis (the fulcrum).** The turn watchdog *looks* like it satisfies the goal —
it times out, fails the task, and notifies the user. It does not, for three reasons:
its **coverage** is philote turns only; its **latency** is minutes; and its
**recovery quality** is eviction without route repair. The missing layer is at the
**router**: an envelope that cannot be delivered *now* should error back *now*, and an
envelope that is delivered but never answered should time out at the router on a
deadline far tighter than the philote's 300 s. We build that layer; we do not add a
fourth reactive watchdog.

---

## Design Principles

1. **One primitive, reused — don't invent a parallel mechanism.** `capability_invoke`
   and Golgi are the *same shape* twice: a pending-envelope registry keyed by id →
   `{reply coords, deadline}` plus a single sweep. Generalize that into the delivery
   path rather than bolting a new subsystem next to it.

2. **Instrument at the chokepoint, not per-call-site.** `ipc.rs` is 31k lines and
   merge-fragile. There are ~24 call sites of `deliver_inbound_task` and one
   `EmitTask` handler. Make the *chokepoint* carry the contract; do not retrofit
   hundreds of call sites.

3. **Split the two failure modes; land the cheap one first.**
   - *Undeliverable-now* — no route / no subscriber / not materializable. Synchronously
     detectable, no timeout needed. Kills the silent-drop black hole.
   - *Delivered-but-silent* — a subscriber accepted the envelope but never answered.
     Needs the timeout + correlation machinery, and overlaps the turn watchdog.

4. **One coherent timeout ladder.** `inner-op < router-envelope < turn-watchdog <
   max-total`. Uncoordinated timeouts cause premature eviction of legitimately-slow
   operations (ONNX 60 s, model retries). Proposed ladder below.

5. **Late-response idempotency.** When the router times out and errors back, the real
   response can still arrive. Every terminal transition is keyed by
   `session_id:turn_id` (or `envelope_id`) and an already-terminal envelope ignores the
   late arrival. (This codebase has a history of `active_incarnation_id` /
   `model_response` rerouting bugs — exactly this hazard.)

6. **Recovery must not depend on the broken transport.** For cross-hotel, the caller's
   deadline is enforced **locally**; it must fire without any remote cooperation,
   because the mesh is precisely what may be broken.

---

## Proposed Architecture

### A. `DeliveryOutcome` — make the chokepoint honest

Change `deliver_inbound_task` from `-> ()` to `-> DeliveryOutcome`:

```rust
pub(crate) enum DeliveryOutcome {
    /// Handed to N live subscribers.
    Delivered { subscriber_count: usize },
    /// No live subscriber for (role, guest). Caller decides: park, materialize, or error back.
    NoSubscriber,
}
```

This is additive and safe: the ~24 existing call sites that ignore the return keep
compiling (no `#[must_use]`). Only the sites that need to react inspect it. This single
change turns every drop site from "silent" into "observable + actionable".

### B. Undeliverable-error-back (Slice 1 — highest leverage, lowest risk)

At the dispatch chokepoint, when delivery yields `NoSubscriber` **and** the target is
not parkable/materializable **and** the envelope carries reply coordinates, synthesize
a terminal failure and deliver it to the reply target:

- **Reply shape that actually unsticks a philote**: a `tool_result` inbound task with
  a non-empty `error` envelope. `handle_tool_result` sets `step_failed =
  error.is_some()`, so a philote in `WaitingTool` resolves the pending tool call
  *immediately* instead of waiting 90 s for the watchdog:

  ```json
  {
    "action": "tool_result",
    "session_id": "...", "turn_id": "...",
    "tool_name": "<original tool>",
    "content": "routing error: no live runner for role '<role>'",
    "error": { "kind": "routing_error", "code": "UNDELIVERABLE",
               "message": "...", "retryable": true, "component": "aiua.router" }
  }
  ```

- **Loop guard**: never error-back an envelope whose own `action` is already a
  terminal reply (`tool_result`, `model_response`, `send_reply`, `*_response`,
  anything carrying `error`). Prevents error-back storms and recursion.

- **Non-agent roles are unambiguously safe**: `resolve_agent_route` returns `Park` for
  materializable agent role-incarnations; a `Deliver` to a non-agent role (tool runner,
  model, capability) that finds no subscriber is genuinely undeliverable — error back.

This is the slice to land and verify first: it directly eliminates the stuck-session
origin, and it establishes the reply plumbing every later slice reuses.

### C. Materialize + notify + wait (Slice 2)

Today `park_and_materialize_local_role` / `park_and_materialize_role_philote` park the
task and trigger materialization fire-and-forget — the caller sees only silence until
either the guest comes up or the watchdog fires. Add:

1. **Immediate "materializing" notice** back to the caller (`materialization_pending`
   status envelope) so the caller knows the route is *in progress*, not dead — and can
   extend its own patience accordingly.
2. **Bounded wait**: the parked task carries a `materialize_deadline` (proposed 30 s).
   A sweep over `parked_inbound` (mirrors `golgi_pipeline_watchdog`) fires
   `UNDELIVERABLE { code: "MATERIALIZATION_TIMEOUT" }` back to the caller if the guest
   has not registered an inbox subscriber by the deadline, and drops the parked entry.
3. On successful materialization + flush, the real response flows normally; the
   deadline entry is cleared.

### D. Per-envelope router deadline (Slice 3 — the "timeout on every envelope")

Generalize the `pending_capability_calls` / `pending_pipelines` registry into a single
**pending-envelope registry** for *request-shaped* envelopes (those carrying reply
coords and expecting a response):

```rust
struct PendingEnvelope {
    reply_to: String, reply_role: String, reply_guest_id: Option<String>,
    session_id: String, turn_id: String, tool_name: Option<String>,
    deadline: Instant, kind: EnvelopeKind, // Tool | Model | Capability | CrossHotel
}
// keyed by session_id:turn_id
```

- On dispatch of a request-shaped envelope, register it with a deadline from the ladder.
- On the matching response (correlated by `session_id:turn_id`), remove it.
- A single sweep (reuse the existing watchdog cadence) fires
  `UNDELIVERABLE { code: "ROUTER_TIMEOUT" }` back to the caller for any envelope past
  its deadline, then removes it. **Idempotent**: if the real response arrives after the
  timeout, the registry no longer has the entry, so it is dropped (or logged as
  late) — never double-delivered.

The router deadline is deliberately **shorter** than the philote turn-watchdog
deadline, so the router recovers first and the watchdog remains a true last resort.

### E. Timeout Ladder (single source of truth)

| Layer | Deadline | Rationale |
|---|---|---|
| Inner op — ONNX inference | 60 s | Existing `capability_invoke` bound |
| Inner op — model HTTP (streaming idle) | 8 s first-token / 120 s total | From `RESILIENT_PHILOTE_LOOP_PROPOSAL` |
| **Router envelope — tool** | **45 s** | New. < WaitingTool 90 s. |
| **Router envelope — model** | **150 s** | New. > 120 s inner total, < WaitingModel 300 s. |
| **Materialization wait** | **30 s** | New. Guest spawn + register + flush. |
| Turn watchdog — WaitingTool | 90 s | Existing backstop |
| Turn watchdog — WaitingModel | 300 s | Existing backstop (escalates tiers, PR #93) |
| Max total active turn | 600 s | Existing hard ceiling |

Every deadline lives in one module (`routing::deadlines`) so drift (like the historic
120-vs-300 s comment mismatch) cannot recur.

---

## Cross-Hotel Nuances

The mesh is **best-effort UDP with no delivery ack**. "Undeliverable" therefore splits:

1. **No route to node** — locally detectable (registry has no live node / no peer
   socket / no advertisement). Error back to the caller *immediately and locally*.
   This is the mesh analogue of Slice 1.

2. **Delivered-to-mesh but dropped remotely** — not locally detectable; only inferable
   by the caller-side envelope deadline (Slice 3). The remote hotel *should* also error
   back if it drops (its own Slice 1), but the caller must not depend on that message —
   **the error reply itself traverses the mesh, and if the mesh is what is broken, the
   error-back never arrives.** Hence the caller-side deadline is enforced locally and
   fires without any remote response.

3. **Remote materialization** — `park_and_materialize_role_philote` runs on the
   *remote* hotel. The origin must receive a `materialization_pending` notice over the
   mesh (best-effort) *and* enforce its own local `materialize_deadline`, so a lost
   notice or a failed remote spawn still resolves to a local `UNDELIVERABLE` rather than
   silence.

4. **Loop / duplicate safety** — cross-hotel error-backs and the existing gossip/relay
   path can re-enter `deliver_event_envelope_or_park`. The node-target guard
   (`target_node != local_node_id → return false`) and the terminal-action loop guard
   (Slice 1) together prevent an error-back from bouncing across the mesh.

---

## Implementation Slices

- **Slice 1 — Undeliverable-error-back (this PR).** `DeliveryOutcome` return type;
  `emit_undeliverable_error_back` helper (terminal-action loop guard + `tool_result`
  error shape); wire at the local `EmitTask` Deliver→NoSubscriber site and the
  cross-hotel drop site. Unit tests: (a) no-subscriber + reply coords → error-back
  delivered and unsticks; (b) delivered normally → no error-back; (c) terminal-action
  envelope → no error-back (no recursion). **Fixes the silent-drop origin of stuck
  tool/model calls.**
- **Slice 2 — Materialize + notify + wait.** `materialization_pending` notice;
  `materialize_deadline` on `ParkedInboundTask`; parked-inbound sweep.
- **Slice 3 — Per-envelope router deadline.** Generalized pending-envelope registry;
  single sweep; idempotent late-response handling; the timeout-ladder module.
- **Slice 4 — Cross-hotel error-back.** No-route-to-node local detection → immediate
  error-back; remote materialization notice over mesh.
- **Slice 5 — Observability.** Structured `delivery_outcome` field on every dispatch;
  `phil hotel status` surfaces pending-envelope count, oldest-envelope age, undeliverable
  counters per role.

---

## Relationship to Existing Work

- **Extends** `RESILIENT_PHILOTE_LOOP_PROPOSAL` (inner-op timeouts + tiered fallback):
  that proposal makes the *model call* resilient; this one makes the *route to and from*
  any component resilient. They share the timeout ladder.
- **Composes with** `codex/stuck-turn-watchdog` (heal-dispatcher zombie scan + startup
  eviction): that work is the reactive cleanup floor; this work is the proactive router
  ceiling. Both should land; neither replaces the other.

---

## Non-Goals

- Guaranteed (at-least-once, acked) mesh delivery — out of scope; the mesh stays
  best-effort and recovery is caller-deadline-driven.
- Model-quality / smart routing — separate concern.
- Persisting the pending-envelope registry across hotel restarts — a restart already
  triggers startup eviction (`stuck-turn-watchdog`) which is the correct floor.

---

## Open Questions

1. Should the router error-back reply text be persona-aware for the philote to
   verbalize, or stay a machine `tool_result` the loop absorbs silently and retries?
   (Lean: machine `tool_result`; the loop decides whether to surface it.)
2. Router-envelope deadlines per-capability vs a single tool/model split — start with
   the split; specialize only if a capability proves to need it.
3. For cross-hotel, do we want an explicit lightweight remote ACK (converting case 2
   into a locally-detectable case 1), or is the caller-side deadline sufficient? (Lean:
   deadline-sufficient for now; ACK is a later optimization.)
