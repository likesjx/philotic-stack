---
name: lifegraph-gardener
description: Use this skill when a philote keeps the operator's LifeGraph pristine — on a schedule or when asked to "garden", "tidy", "audit", "check integrity/connectedness", or "dedupe" the graph. It runs graph science over the whole graph (components, orphans, hubs, semantic duplicates, stale loops, temporal and conformance checks) and applies one governed, provenance-stamped fix per step. It never deletes a lived fact and never asks the operator to run a script.
catalog:
  skill_name: lifegraph.gardener
  implied_tools:
    - life.audit
    - life.tidy
    - life.list
    - life.view.neighborhood
    - life.recall
    - life.commit
    - life.resolve
    - life.ontology
  validation_state: proposed
  skill_markers:
    - governed
    - life_graph
    - never_delete
  field_sources:
    required_fields: []
    optional_fields:
      - labels
      - max_actions
      - duplicate_similarity
      - stale_days
    repo_skill_path: skills/lifegraph-gardener/SKILL.md
    workflow: "life.audit -> plan one life.tidy step per suggested action -> execute -> life.audit again -> report delta; needs_judgment items go to the operator with evidence"
---

# LifeGraph Gardener

The operator's rule (2026-09-12): the philotes keep the LifeGraph pristine themselves. No
operator-run scripts. Use as much graph science as possible. Completed things are
**resolved or retired, never deleted**.

## Purpose

`life.audit` is a read-only graph-science pass over the whole LifeGraph:

- **Connectedness** — weakly connected components, the giant component, small islands,
  and live orphans (degree 0). A LifeGraph is only useful when a loop, a person, a
  place, and an event can be reached from each other; 68% orphans (the 2026-09-14
  baseline) means recall cannot walk anywhere.
- **Hubs** — PageRank and degree, so the gardener knows what the graph is organised
  around (roles, the operator, recurring people) and where a new node should attach.
- **Duplicates** — same label, both live, either identical normalized summaries or
  embedding cosine similarity above the threshold (default 0.90). The keeper is the
  confirmed node, else the newest; the other is proposed for retirement under it.
- **Stale loops** — live OpenLoop / Commitment / NextAction / Goal / Project past their
  date, or untouched for `stale_days` (default 45).
- **Temporal** — dated labels with no date, nodes with no `observed_at`.
- **Conformance** — missing or bare-numeric ids, unknown validation states, edges
  outside the vocabulary.

It returns `suggested_actions` (deterministic, one `life.tidy` call each, priority
order) and `needs_judgment` (things only evidence or the operator can settle).

`life.tidy` applies exactly one action: `retire_duplicate` (retire under a keeper with
a `SUPERSEDES` edge), `link` (MERGE an edge from the observe or gardening vocabulary),
`resolve` (close a loop with a note), `retire` (a stray that never was a lived fact).
Every touched node and edge is stamped `tidied_at`, `tidied_by`, `tidy_reason`.
Confirmed nodes are never retired or resolved without `operator_approved`.

## Workflow

1. `life.audit` (optionally scoped by `labels`). Read `health_score`, `live_orphans`,
   `components`, `duplicates`, `suggested_actions`, `needs_judgment`.
2. Declare an `active_plan` with **one step per suggested action**, each bound to
   `life.tidy` with `{"action": <the entry verbatim>}`. Do not bundle. Do not invent ids.
3. Execute the steps in order. A failed step (node vanished, confirmed target) is
   reported, not retried blindly.
4. For `needs_judgment`: a stale loop is resolved only with evidence (recall the loop,
   check for an outcome event); otherwise ask the operator in one short list. Islands
   are linked to the hub they belong to (`life.view.neighborhood` to decide), or left
   and reported.
5. Run `life.audit` again and report the delta: score before/after, actions applied,
   what needs the operator. Keep the report to what changed.

## What the harness does for you

When a `life.audit` result carries `suggested_actions`, the harness seeds your `active_plan`
with one `life.tidy` step per action (up to 12 per pass) and a closing `life.audit` step.
Execute them in order; do not re-plan, do not skip to a summary, do not stop to ask
whether to continue — the evaluator verifies each step by its tool result and the
continuation loop carries the rest. Your reply reports what landed; the harness appends a
receipt of every write this turn, so never describe future work as if it were done.

## Edge types outside the vocabulary

`conformance_issues` of kind `unknown_rel_type` (for example `HAS_SUB_ROLE`, `SUB_ROLE_OF`,
`HAS_SECTION` written by another philote) are a registration gap, not a defect to rewire.
Propose the type as an ontology extension with `life.patch.propose`, or list the distinct
types for the operator. Never replace or remove those edges.

## Guardrails

- Never delete. Never `retire` or `resolve` a confirmed node without operator approval.
- Never fabricate an id: every id in a tidy action must come from `life.audit`,
  `life.list`, `life.recall`, or the operator.
- One action per tool call; one step per action. The plan evaluator verifies each.
- Prefer linking to the anchor role only when nothing more specific applies; a person
  met at a place belongs to the event, a commitment to its loop.
- Graph science first, judgment second: the report decides; the model explains.

## Scheduling

Register a daily gardening pass with `cron.register` (session target isolated, quiet
hours) whose prompt is: "Run the LifeGraph gardener: life.audit, then one life.tidy
step per suggested action, then life.audit again; send the operator the delta only if
anything changed or needs judgment."
