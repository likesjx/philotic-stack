---
title: Autopoiesis — Closing the Loops of a Self-Building World
doc_type: proposal
domain: operator-control-plane
status: active
disposition: proposed
last_updated: 2026-07-07
tags:
- autopoiesis
- self-building
- gated-autonomy
- feedback-loops
- north-star
related_docs:
- LIFE_GRAPH_OS_PROPOSAL.md
- AGENT_RESOURCE_MODEL_PROPOSAL.md
- ARCH_RULES.md
task_refs:
- docs/task.md
---

# Autopoiesis — Closing the Loops of a Self-Building World

> **Operator intent (2026-07-07, verbatim):** "I really want this philotic system
> to be a self-building world."

## Goal

Philotic already contains every organ of a self-building system — a repair
dispatcher, a retrieval feedback engine, an attention steward, a project
intelligence graph with a full proposal→seam→slice process, and agents capable
of authoring and shipping code. What it lacks is closure: **every loop in the
system currently terminates in prose or in a human.**

- `life.recall.feedback` generates a `SystemPatch` *description* — and stops.
- The heal queue classifies failures and restarts guests — but a *recurring*
  failure pattern never becomes a filed fix.
- The attention steward evaluates signals — observe-only, forever, because its
  activation gate exists only as proposal prose.
- The SVE process holds 200+ seams — every one waiting for a human to notice it.

The goal of this epic: **connect each loop's last mile, under earned,
per-lane autonomy** — so the system converges toward noticing its own gaps,
proposing its own work, and executing the safe subset without asking.

## Core Recommendation

Generalize the pattern the codebase already trusts. Three precedents prove it:

1. **`PatchRisk` gating** (`data-memorygraphrag`): Low → auto-apply with audit,
   Medium → confirm-first, High → proposal-only.
2. **Respawn budgets** (`guest_manager`): autonomy with a flap ceiling and a
   visible escalation when the budget exhausts.
3. **Tool pre-approval promotion** (`philote` approval policy): a success
   streak earns standing approval; a failure resets it.

Fuse these into one first-class concept — the **AutonomyGrant** — attached to
each self-building lane:

```
AutonomyGrant {
  lane: "graph.bridge_edges" | "fleet.heal_slices" | "work.file_proposals" | ...,
  posture: ProposalOnly | ConfirmFirst | AutoWithAudit,
  budget: { max_actions_per_day, max_consecutive_failures },
  earned: { promotions require N confirmed-good outcomes; any operator
            reversal demotes one posture level },
  audit: every autonomous action writes a decision record (intel graph or
         LifeGraph Signal) with what/why/evidence — reviewable, reversible,
  kill_switch: per-lane env override, always
}
```

The operator approves **postures, not patches**. Trust is earned per-lane,
never global, and demotion is automatic on reversal.

## The Loops, and Their Last Miles

| # | Loop | Today ends at | Closed means | Lane |
|---|------|---------------|--------------|------|
| 1 | Retrieval feedback → graph structure | prose `SystemPatch` node | Low-risk patches (bridge `RELATES_TO` edges between the feedback's connected candidates) actually MERGE into Memgraph, audited | `graph.bridge_edges` |
| 2 | Heal queue → fleet fixes | restart / escalate | A recurring pattern (same `pattern_tag` ≥ N times / window) auto-files an intel-graph proposal with journal evidence attached; coding agents pick it up | `fleet.heal_slices` |
| 3 | Observation → work | human reads dashboards | Aria's architect charter: sweep heal queue, error logs, defect ledger, seam staleness on a cadence; author scored proposals into the intel graph | `work.file_proposals` |
| 4 | Steward signals → operator nudges | observe-only policy | The "5 confirmed SIL entries" gate implemented in code: confirmed entries unlock bounded active check-ins (max/day budget) | `steward.active_checkins` |
| 5 | Proposal → implementation | operator triggers a session | Scheduled agent sessions claim the top-scored open proposal within granted lanes and execute the SVE loop end-to-end | `work.execute_slices` |

Loop 5 is deliberately last: it is the full autopoietic cycle, and it should
only run after lanes 1–4 have produced the track record that earns it.

## Slices

| Slice | Content | Size | Verify |
|---|---|---|---|
| A1 `autonomy-grant-core` | `AutonomyGrant` record (graph-backed, per-lane), posture state machine with earn/demote transitions, audit-record writer, env kill switches. Pure library + storage; no lane wired yet. | M | test-green |
| A2 `feedback-to-action` | Wire `recall_feedback_patch_proposal`'s SafeAutoUpdate cases through lane `graph.bridge_edges`: disconnected/missing feedback with clear candidates writes the bridge edge (living-cycle vocabulary, idempotent MERGE) instead of stopping at prose. ConfirmFirst posture by default. | M | test-green + live feedback round-trip |
| A3 `heal-pattern-filing` | heal-dispatcher counts `pattern_tag` recurrences; threshold breach files an intel-graph proposal (REST :8900) with the last N raw log lines as evidence. ProposalOnly posture — filing IS the action. | S–M | smoke-green (induced recurring failure files exactly one proposal) |
| A4 `aria-architect-charter` | Role manifest + daily cron for Aria: sweep heal queue / DEFECTS / seam staleness, author or update scored proposals, morning dev-brief to operator. Config-shaped, mirrors Beacon's chief-of-staff charter. | S | watched-live (first authored proposal reviewed by operator) |
| A5 `steward-activation-gate` | Implement the SIL confirmation counter and bounded active check-ins behind lane `steward.active_checkins`, ConfirmFirst until 5 confirmed entries, then AutoWithAudit with a max-per-day budget. | M | test-green + live confirmation cycle |
| A6 `scheduled-slice-executor` | A scheduled session (cloud or cron) claims the top open proposal in granted lanes, runs the SVE loop (worktree → slice → verify → PR), reports to operator. Starts ProposalOnly (drafts the plan, does not merge). | L | watched-live, operator reviews first N runs |

Dependency: A1 → {A2, A3, A5}; A4 independent (config); A6 after A2–A4 have
produced ≥ 2 weeks of clean audit records.

**Prerequisite:** the LifeGraph epic's in-flight slices (retrieval lane 3–4,
charter, hygiene, auto-capture) land first — a self-building world needs its
world-model working. A2 specifically builds on the retrieval lane's Slice 4
(Muninn provenance) landing.

## Autonomy Contract (standing rules)

1. Every autonomous action is **auditable** (decision record with evidence),
   **reversible** (soft-retire / revert path named in the audit record), and
   **budgeted** (daily caps; consecutive-failure ceilings).
2. Postures only promote on **operator-confirmed outcomes**, and demote
   automatically on any operator reversal. No lane ever starts above
   ConfirmFirst.
3. Per-lane kill switches (`PHILOTIC_AUTONOMY_DISABLE_<LANE>`) override
   everything, always.
4. The intel graph is the ledger of self-directed work; the LifeGraph is the
   ledger of operator-world effects. An action that touches neither did not
   happen — and is therefore forbidden.

## Disposition

`proposed` — awaiting operator review of this document. Current slice on
acceptance: **A1 `autonomy-grant-core`**, immediately followed by **A2
`feedback-to-action`** as the first loop the system closes by itself.
