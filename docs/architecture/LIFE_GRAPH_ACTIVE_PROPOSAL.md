# Life Graph Active — From Passive Record to Life Manager

> **Operator intent (2026-07-23, paraphrased):** "The lifegraph has become the focus.
> I need it to be able to manage my life — let's make it more active."

Status: **proposal — slices S1/S2 implementation started on this branch**

## Goal

The LifeGraph substrate is strong and live: 25 node labels with provenance envelopes and a
`proposed → confirmed` validation lifecycle, 5 semantic vector spaces, an automatic
auto-capture lane forking lived facts out of agent turns, auto-recall injection, hygiene +
feedback-ranking maintenance (recall_utility EWMA, PR #321). But everything user-facing is
**pull-based**. The graph records the operator's life; it does not yet help run it.

This proposal sequences the minimum work to make the graph *active* — able to compute and
deliver an agenda — while honoring the Attention Steward's core covenant: **interruption
rights are earned through observed evidence, never assumed**
(`docs/architecture/life-graph/ATTENTION_STEWARD.md`).

## Why these slices, in this order

1. An active system that silently drops observations destroys trust immediately →
   ingestion reliability first (S1).
2. An agenda must be *computable*: "what advances which goal, what's blocked, what was
   promised to whom" requires traversable edges. Goal/Commitment/OpenLoop/NextAction nodes
   exist, but the agenda edge types documented in
   `LIFE_GRAPH_SCHEMA.md` § Relationship Types are rejected by the closed write vocabulary
   (`LIVING_CYCLE_REL_TYPES`, `crates/data-memorygraphrag/src/cypher.rs`) → open the
   vocabulary, typed and validated (S2).
3. The first active surface must be operator-invited, scheduled, and feedback-instrumented —
   a daily brief, not an interruption (S3).
4. Brief feedback is exactly the SIL evidence the Attention Steward's active-gate was
   designed to wait for → unlock targeted nudges through the existing gate, not around
   it (S4).

## Slices

### S1 — `life.observe.batch` isolation repro + fix

Open since the 2026-07-14 handoff (`docs/HANDOFF-2026-07-14-lifegraph-batch.md`). Three
layers already fixed (routing PR #271, pool churn PR #275/#277, embed timeout); observations
(flight itineraries) still never land — **in batch OR single writes**, so the fault may be
observation-content-specific rather than batch machinery.

- Extend `crates/philotic-client/examples/life_graph_ipc_smoke_driver.rs` into a
  **multi-item batch driver** that sends `life.observe.batch` directly at the runner over
  IPC — bypassing philote, the model, the WaitingTool watchdog, and cross-hotel noise —
  and reports each item's write/reject/error individually.
- Include a reconstruction of the failing flight-itinerary observations as a fixture, so
  content-specific rejection (label validation, edge validation, property shape) is
  distinguishable from transport/batch failure in one run.
- Fix whatever the repro isolates; add a regression test at that layer.
- **Verification:** repro driver green against a live runner with the previously-failing
  payload shapes; node count observed to increase by exactly the batch size.

**LIVE RESULT (2026-07-24, vps-jane, run 421aadc8):** the defect did **not reproduce**.
All flight/Mali fixture shapes landed with `embed=ok` in BOTH single and batch phases
against the deployed develop runner — the three already-shipped layers (batch routing
PR #271, pool churn PR #275/#277, embed timeout) evidently resolved it. The only
failures were the agenda-edge fixture, correctly rejected by plan validation because the
deployed runner predates S2 (`ADVANCES … not a living-cycle relation`) — itself a live
proof that unknown rel_types are rejected before the node write. The driver now serves
as the regression harness; re-run after S2 deploys to confirm the agenda-edge fixture
flips to `proposed` + `target_missing`. Test nodes (`life:isolation:*`) were deleted
from the live graph after the run.

### S2 — Agenda edge vocabulary (typed, validated, still closed)

Add the schema-documented agenda relationships to the enforced `life.observe` edge write
vocabulary:

| rel_type | endpoints (validated) |
|---|---|
| `ADVANCES` | NextAction/Habit/Project → Goal |
| `BLOCKED_BY` | Goal/NextAction/Project → Concern/OpenLoop/Commitment |
| `NEEDS_FOLLOWUP` | Event/Commitment/OpenLoop → NextAction/Commitment |
| `PROMISED_TO` | Commitment → Person |
| `CONTAINS` | Project/System/Routine → NextAction/Habit/OpenLoop |
| `SUPPORTS` | System/Habit/Routine → Goal/Habit |

Design constraints:

- The vocabulary **stays closed** — these six join `LIVING_CYCLE_REL_TYPES`' six; unknown
  rel_types are still rejected before the node write.
- New: **endpoint label validation** per the schema table above (the existing six are
  endpoint-unvalidated; the new six must not be, or agents will wire junk topology).
  Validation failure rejects the edge with a typed reason, not the whole observation.
- `SCOPED_TO` remains the only server-injected rel type; nothing here is written
  server-side.
- Provenance envelope unchanged; new edges enter with the node's `validation_state`.
- **Verification:** unit tests over compile/reject paths per rel_type × endpoint matrix;
  IPC smoke driver extended with one agenda-edge observation.

### S3 — Daily brief (first active surface, operator-invited)

A scheduled digest is an **operator-approved standing request**, not an autonomous
interruption — the Attention Steward anti-nag policy is not violated and its active gate is
not bypassed.

- Cron (existing paracrine heartbeat machinery,
  `crates/aiua/src/service/cron_ticker.rs`) triggers the `chief_of_staff` domain steward
  (Beacon, `life:role:chief-of-staff`, `crates/data-memorygraphrag/src/zoning.rs`) each
  morning.
- Brief content computed from `life.recall` over active Goal / Commitment / OpenLoop /
  NextAction scoped to their domains, now traversable via S2 edges: due/overdue
  commitments, stale open loops, next actions that `ADVANCES` an active goal, blocked
  items.
- Delivery over the Telegram hotel where Beacon already runs (the cron→routing-fields path
  is proven in develop's `cron_ticker.rs` tests).
- Every brief line carries a feedback affordance; operator reactions flow into
  `life.recall.feedback` (feeding the recall_utility EWMA) **and** are recorded as
  Attention Steward observations (SIL evidence).
- **Verification:** brief fires from cron on schedule; a rated brief line demonstrably
  changes `recall_utility` on the referenced node; SIL entry count increases.

### S4 — Attention Steward active unlock (through the gate, not around it)

No new machinery — this slice *exercises* `ATTENTION_STEWARD.md`'s designed unlock path
using S3's evidence stream:

- Brief feedback accumulates SIL entries through the reinforcement loop.
- When ≥5 SIL entries reach `active` AND the operator explicitly approves the first active
  entry AND the relevant `AttentionPatch` carries `risk_tier: confirm_first` or lower,
  targeted real-time nudges (e.g. `commitment_approaching`/`commitment_overdue`) may fire
  over the same Telegram delivery path.
- **Verification:** gate conditions checked in code with tests; first live nudge only
  after recorded operator approval.

## Out of scope (tracked in LIFE_GRAPH_OS_PROPOSAL.md backlog)

Runtime conflict detection wiring, background drift detector, growth-loop role, and the
single-Memgraph availability question (hot-path consumers must keep degrading gracefully
when vps-jane is unreachable).

## Repo hygiene precondition (operator action)

`origin/main` is vestigial: 577 behind develop with 3 unique commits, all superseded (the
PR #90 cron-routing fix was silently stranded there and later re-implemented in develop).
Recommend deleting `main` or hard-resetting it to `develop` and making `develop` the GitHub
default branch, so no future fix can strand.
