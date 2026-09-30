---
title: Lyra — Travel Specialist Agent
doc_type: proposal
domain: runtime-sessions
status: active
last_updated: 2026-08-09
tags:
- lyra
- travel
- agent-design
- aria-idea-pipeline
related_docs:
- ARIA_IDEA_PIPELINE_PROPOSAL.md
- AGENT_INCARNATION_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: lyra-travel-agent
implements: []
implemented_by:
- crates/aiua/src/lyra_charter.rs
active_seams:
- lyra-travel-agent
source_of_truth_targets:
- docs/task.md
---

# Lyra — Travel Specialist Agent

> Provenance: the FIRST idea promoted through the Aria idea pipeline
> (captured from the operator via Telegram, watched-live 2026-07-20, as
> `idea:lyra-travel-agent`). Both the original LifeGraph idea node (DEF-075
> Memgraph wipe) and the original graph-only proposal node (DEF-072
> rescan-wipe) were destroyed in late July 2026 and restored on 2026-08-03 —
> this file exists so no rescan can eat the proposal again. Promoted ideas
> must be file-backed from now on.

## Goal

A travel specialist agent, **Lyra**, that plans, books-supports, and stewards
the operator's travel end to end, structured as one persona with three role
incarnations:

- `lyra:vera` — research and options: destinations, routes, fares, lodging
  candidates, constraint gathering (dates, budget, loyalty programs).
- `lyra:atlas` — logistics: itineraries as LifeGraph structure (`Commitment` /
  `Event` / `NextAction` nodes with due times), document/checklist tracking,
  connection-risk awareness.
- `lyra:astra` — in-trip steward: day-of re-entry context, changes and
  disruptions, "what's next" answers, post-trip capture back into the
  LifeGraph.

## Disposition

`implemented` — slice 1 (charter seeding) merged via PR #420 and **activated
on mbp-jane 2026-08-09, smoke-green**: `agent-lyra` seeded (identity,
orchestrator + the three incarnations on the `travel` profile), philote
guest registered live with the 4-role delegation roster, and the Muninn
vault `self_agent-lyra` provisioned on the Cortex (local observers reject
vault-creation writes — see the tunnel workaround in
[docs/task.md](../task.md)). The idea's LifeGraph anchor
`idea:lyra-travel-agent` is `shipped`. Watched-live validation (a real
research→structure→steward trip pass) is the remaining rung; deferred seams
(Vera web tooling, Astra heartbeat, direct Telegram surface) are named in
the task list.

## Current Slice

Slice 1 — charter seeding (`crates/aiua/src/lyra_charter.rs`, seam
`lyra-travel-agent`):

- `lyra_charter::ensure_roles` seeds the three incarnations (`vera`,
  `atlas`, `astra`) idempotently (create-if-absent; live
  `role.create_or_update` edits survive restarts — same reconciliation
  contract as `architect_charter`). Opt-in per hotel via
  `PHILOTIC_LYRA_CHARTER_ENABLED` + `PHILOTIC_LYRA_AGENT`; no persona or
  hotel is hardcoded.
- A `travel` toolset profile (`main.rs::seed_toolset_profiles`) grants the
  `life_graph` + `memory` classes and `capability.request`; a pinning test
  asserts every tool the charters name is actually granted.
- No cron is registered — Lyra activates via normal `/role` switching and
  `handoff.to_role`.

Work items live in [docs/task.md](../task.md) § "New Project: Lyra Travel
Specialist" (activation smoke, watched-live pass, idea-sweep ship closure).

## Design Notes

- Persona + roles follow the standard agent-incarnation model (single-active
  invariant; `/role` switching). **Decided (slice 1):** the durable seed is
  code (`lyra_charter.rs`, mirroring `architect_charter.rs`) rather than a
  one-off `role.configure` session, so a fresh hotel materializes Lyra from
  env config alone; `role.configure`/`role.create_or_update` remain the
  live-tuning path and their edits are never clobbered by the seed.
- Travel state lives in the LifeGraph (trips as `Project` containing
  `Commitment`/`Event`/`NextAction`), not in a bespoke store — the Life
  lenses and Attention Steward then work for travel for free. The charters
  instruct `life.observe`/`life.recall`/`life.commit`/`life.patch.propose`,
  and the `travel` profile's `life_graph` class grant makes those tools
  project (and route to the remote life-graph-runner where
  `PHILOTIC_REMOTE_LIFE_GRAPH_RUNNER_NODE` is set).
- Booking-adjacent actions are approval-gated per the operator-identity
  ceremonies proposal; Lyra proposes, the operator confirms. The gate is a
  shared-posture block in every incarnation's manifest, plus the standing
  server-side approval policy.
- **Hotel homing — resolved config-shaped:** no hotel is hardcoded; the
  operator sets the two env vars on exactly the hotel that should own the
  incarnations (and declares the Lyra agent in that hotel's mesh-config
  `agents` stanza). Recommendation: mbp-jane alongside Aria, since idea
  intake and the operator's Telegram surface already live there.
- **Deferred, named in the charters:** web-search/fare tooling for Vera
  (charter says to `capability.request`, never fabricate); Astra's paracrine
  heartbeat subscription for day-of nudges (Astra is reactive this slice and
  says so).
