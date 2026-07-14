---
title: Aria Idea Pipeline — Operator Ideas to Implemented Slices
doc_type: proposal
domain: product-management-plane
status: proposed
last_updated: 2026-07-14
tags:
- idea-intake
- aria
- lifegraph
- intel-graph
- agent-workflow
- operator-experience
related_docs:
- LIFE_GRAPH_OS_PROPOSAL.md
- NATIVE_APPLE_APP_PROPOSAL.md
- GRAPH_INTELLIGENCE_PROPOSAL.md
- AGENT_WORKFLOW_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: aria-idea-pipeline
implements: []
implemented_by: []
active_seams:
- idea-intake-charter
- idea-triage-sweep
- idea-closure-loop
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- docs/task.md
---

# Aria Idea Pipeline — Operator Ideas to Implemented Slices

## Goal

Let the operator text Aria an idea — "I need HealthKit pulling my data into the
lifegraph" — and have it travel, with provenance, from that message to an
implemented, merged slice:

```text
operator → Aria (Telegram)          idea captured as a LifeGraph node
        → idea lens / triage sweep   surfaced to coding sessions at bootstrap
        → intel-graph proposal/seam  promoted into the normal work pipeline
        → graph_next_task → slice    implemented, PR'd, merged
        → closure                    idea node marked shipped; Aria tells the operator
```

No new subsystems: the pipeline composes three planes that are all live as of
the 2026-07-14 fleet deploy — LifeGraph writes (`life.observe` /
`life.observe.batch`, remote-runner-bound from every hotel), the LifeGraph
read plane (lenses in the app, `life.recall` on the agent side), and the
intel-graph work pipeline (`graph_create_node` → `graph_next_task` →
`session_start` → PR → `graph_record_test_run`).

## Core Recommendation

Model ideas as **LifeGraph `GrowthHypothesis` nodes** (no schema patch needed)
tagged for retrieval, written by Aria at intake, and **promoted by a coding
session** into intel-graph proposals during a bootstrap "idea sweep". The
LifeGraph node remains the provenance anchor for the idea's whole life;
the intel-graph node owns execution state, exactly per the canonical
ownership split in LIFE_GRAPH_OS_PROPOSAL.

### Idea node shape (convention, not schema)

- label: `GrowthHypothesis`
- `id`: `idea:<slug>` (e.g. `idea:healthkit-observe`)
- `claim_summary`: the idea in one or two sentences, operator's words preserved
  in the evidence packet
- tags via properties: `idea_kind: implementation`, `target: philotic-stack`
  (other targets later: `muninndb`, `home`, …)
- lifecycle property `idea_status`: `captured` → `promoted` (with
  `graph_ref: doc:<proposal-id>`) → `shipped` | `declined` (with reason)
- provenance envelope as always: `source_membrane: telegram`,
  `observed_by: aria`, `validation_state: proposed`

## Stage 1 — `idea-intake-charter` (Aria)

- Extend Aria's role with the idea-steward behavior (charter text on her role
  or a dedicated `idea.steward` skill riding the existing `life.steward`
  grants — she needs nothing beyond `life.observe`, `life.observe.batch`,
  `life.recall`):
  - when the operator expresses a want/need/idea for a capability, capture it
    as an idea node (batch when several arrive at once), echo back the
    captured summary + id, and ask at most ONE clarifying question — capture
    first, refine later. Never silently drop an idea.
  - on "what ideas are pending?", answer from `life.recall` over the idea tag.
- **Tool-projection lesson (from the Coach incident, 2026-07-14): the
  relevance heuristic must fire on idea language.** Extend
  `skill_is_relevant_for_turn` for `life.steward` with: "idea", "implement",
  "build me", "i need", "feature", "capture this", "add to the backlog".
  Without this, Aria's tools are not projected and the charter is dead text.
- Verification: watched-live — operator texts Aria an idea, the node appears
  in Memgraph and in the app's Life surface.

## Stage 2 — `idea-triage-sweep` (coding sessions)

- Add an **idea sweep** step to the session-bootstrap path of coding agents
  (`skills/graph-intelligence` + `session-hygiene`, and AGENTS.md §Bootstrap):
  after Muninn recall, query pending ideas (`life.recall` with idea-tag
  context, or the edge lens REST — both live) and surface them in the
  orientation summary.
- Triage disposition per idea (operator approves promotion unless they have
  granted standing approval for a target):
  - **promote**: `graph_create_node` an intel-graph proposal (or a seam on an
    existing proposal — e.g. HealthKit promotes onto
    `doc:native-apple-app-proposal` as slice 5 rather than a new doc), then
    update the idea node: `idea_status: promoted`, `graph_ref`.
  - **decline / defer**: `idea_status` updated with the reason — the answer
    must reach Aria so the operator hears why, not silence.
- Verification: an idea captured in Stage 1 shows up in a fresh session's
  orientation and round-trips to `promoted` with a real graph node.

## Stage 3 — `idea-closure-loop`

- When a promoted idea's slice merges (session closeout), the closing session
  sets `idea_status: shipped` on the LifeGraph node and emits the existing
  change-push (`LifeGraphChange`) — the operator's app badges it live.
- Aria's charter includes closure delivery: on her next relevant turn (or a
  nightly cron digest later), she tells the operator what shipped from their
  ideas. Anti-nagging policy applies: digest, not per-merge pings.
- Deferred (own slice, only if the manual loop proves too slow): a cron →
  paracrine `idea_digest` signal to Beacon for cross-domain steward triage.

## First Idea Through the Pipe: `healthkit-observe`

Queue this proposal's own test cargo. Sketch (implementation detail lives with
the slice, on `doc:native-apple-app-proposal`):

- App-side `ToolHost` gains a HealthKit source (first *sensor*, sibling of the
  EventKit tool plane): `HKObserverQuery` + anchored queries for sleep,
  workouts, HRV, resting HR, steps; explicit per-metric operator toggles;
  OS permission prompts.
- Batching: samples accumulate and flush via **`life.observe.batch`** (≤25
  per call — built for exactly this) as `Signal`/`Event` nodes,
  `source_membrane: edge:ios-healthkit`, one summary node per day per metric
  rather than raw-sample spam; raw detail stays on-device.
- Read side is free: the Life lenses and `cross_domain_entanglement` recall
  strategy already consume these nodes ("are sleep and rowing entangled?").
- Requires no server work beyond what shipped 2026-07-14.

## Disposition

`proposed`

## Current Slice (for the picking-up session)

Smallest honest slice = Stage 1 end-to-end:

1. Relevance keywords for idea language in `skill_is_relevant_for_turn`
   (`crates/philote/src/catalog.rs` — one match-arm edit + tests).
2. Aria charter update (seeded role/skill text, `crates/aiua/src/main.rs`
   life.steward record or Aria's role config — reseeds at boot, per the
   2026-07-13 batch slice precedent).
3. Idea-node convention doc'd in LIFE_GRAPH_SCHEMA notes (no migration).
4. Deploy mbp-jane (Aria's hotel) + watched-live: operator texts an idea,
   node lands, Life tab shows it.

Out of scope for slice 1: the triage sweep skill edits (slice 2), closure
loop (slice 3), HealthKit itself (promoted as the first idea, implemented
under the apple proposal).

## Open Questions

- Standing promotion approval: should `target: philotic-stack` ideas
  auto-promote to intel-graph without per-idea operator sign-off? (Recommend:
  manual for the first ~10, then decide with evidence.)
- Dedicated `Idea` label vs `GrowthHypothesis` + properties: revisit only if
  lens ergonomics demand it — a label change is a governed SchemaPatch.
- Cross-hotel reliability: Coach's 2026-07-14 `stuck_turn_evicted:WaitingTool`
  on cross-hotel lifegraph calls is an open watch-item; if Aria (mbp) hits it,
  the intake charter needs a retry note and the defect gets priority.
