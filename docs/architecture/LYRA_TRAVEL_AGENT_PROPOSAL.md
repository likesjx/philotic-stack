---
title: Lyra — Travel Specialist Agent
doc_type: proposal
domain: runtime-sessions
status: proposed
last_updated: 2026-08-03
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
implemented_by: []
active_seams: []
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

`proposed` — awaiting design + implementation. The idea's LifeGraph anchor is
`idea:lyra-travel-agent` (`idea_status: promoted`, `graph_ref` → this
proposal); ship-time closure flows through `just idea-sweep ship` per the
Aria idea pipeline.

## Design Notes (to be expanded by the implementing session)

- Persona + roles follow the standard agent-incarnation model (single-active
  invariant; `/role` switching); seed via `role.configure` with the
  `role-authoring` skill.
- Travel state lives in the LifeGraph (trips as `Project` containing
  `Commitment`/`Event`/`NextAction`), not in a bespoke store — the Life
  lenses and Attention Steward then work for travel for free.
- Booking-adjacent actions are approval-gated per the operator-identity
  ceremonies proposal; Lyra proposes, the operator confirms.
- Open questions: which hotel homes Lyra (mbp with Aria vs vps with Beacon);
  web-search/tooling grants for Vera; whether Astra subscribes to paracrine
  heartbeats for day-of nudges.
