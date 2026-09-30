---
title: Procedural Graphs — Learned Execution Structure Under Grounded Verification
doc_type: proposal
domain: runtime-sessions
status: accepted-current-slice
disposition: accepted-current-slice
last_updated: 2026-09-12
tags:
  - procedural-graphs
  - plan-eval
  - skills
  - skilldag
  - self-improvement
  - distill
  - autopoiesis
  - guidance
related_docs:
  - SELF_IMPROVEMENT_LOOP_PROPOSAL.md
  - COGNITIVE_LOOP_V2_PROPOSAL.md
  - AUTOPOIESIS_PROPOSAL.md
  - GRAPH_DOORS_AND_LIFE_CORE_PROPOSAL.md
  - SKILL_GOVERNANCE_HARDENING_PROPOSAL.md
  - DATA_DRIVEN_TOOL_GRANTS_PROPOSAL.md
  - WHISPER_PROTOCOL_PROPOSAL.md
  - SCRIPTED_TURN_LOOP_PROPOSAL.md
  - ARCHITECTURE_STATUS.md
task_refs:
  - docs/task.md
proposal_id: procedural-graphs
implements: []
implemented_by: []
active_seams:
  - procedure-graph-record
  - procedure-run-ledger
  - procedure-localized-guidance
  - procedure-seeded-plans
  - procedure-refiner-gate
  - procedure-generative-guidance
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
---

# Procedural Graphs

**Why now:** Google's *Procedural Graphs: Self-Evolving Execution Structures
for LLM Agents* (Lu, Chen, Wu, Arık et al., arXiv 2609.09153, 2026-09-09)
stores *what to do* as a small typed graph instead of *what happened* as
episodes, and reports first or joint-first in 21 of 24 model × benchmark
settings against six memory-based baselines (MemoryBank, RAP, ExpeL,
AutoGuide, AWM, KnowAgent). The mechanism is three parts: a graph of
`(procedure, relation, procedure)` triplets whose edges carry
`condition / guidance / pitfalls`; per-step **localization** (exact-match the
last tool call to a node, pull a 2-hop neighbourhood, render it as advice
that biases but never dictates the next action); and **validation-gated
self-evolution** (a refiner contrasts failed with successful trajectories,
proposes add/delete edits, and the candidate graph is kept only if a held-out
score does not drop; rejected edits are retained as negative evidence).

A structural map of Philotic against the paper on develop `85dd312f`
(2026-09-12) found that **Philotic has every organ except the graph**:

| Paper component | Philotic today | State |
|---|---|---|
| Held-out validator | `plan_eval::verify_plan_steps` — a step settles only against a real tool result; `CarryoverPlan.verified_step_ids` refuses model self-certification (`philote/src/session/types.rs`) | built, stronger than the paper's |
| Refiner harness | `distill.rs` — bounded lookaside whisper, fixed tool allowlist, `Discard` routing, budgeted lane | built; contrasts error-then-success *inside one turn* only |
| Rejected-edit memory | `AgentReflexPreference` suppression keeps an operator-rejected preference instead of deleting it (`aiua/src/service/ipc.rs`, `RecordRoleHandoffReflexEvidence`) | built for one edge type (role handoff) |
| Guidance injection slot | `plan_directive`, `plan_continuation_brief`, `reentry_hint` (`philote/src/plan_eval.rs`) | built; all static English |
| Harness-authored plans | `SessionState::seed_outcome_plan` (`philote/src/session/mod.rs`) | built; two hard-coded shapes |
| Procedure topology | — | **missing** |
| Trajectory accumulation | `PlanEvalOutcome::event_json` | emitted as a log event, then discarded |

Two things must be said plainly because the names invite the wrong reading.
**SkillDAG is not a procedure graph**: `AbstractSkillRecord.allowed_skills`
is a transitive *tool-grant* closure with untyped edges inside a JSON blob,
walked once at session bind to decide which tools appear. **LifeGraph
`Habit`/`Routine` are not procedures either**: they model the operator's
behavioural loops as observed facts; nothing in the turn loop reads them as
advice. Philotic's actual procedures live today as prose in
`skills/*/SKILL.md`, which is exactly the artifact the paper's graph
replaces.

## What this proposal borrows

| Paper | Philotic form |
|---|---|
| Graph `G = (V, R, E, Φ)`; edges carry condition/guidance/pitfalls | `ProcedureGraphRecord` in the hotel context graph, node kind `procedure`, ≤ 64 nodes, tool-bound nodes only at first |
| Localization by exact match of the last action | philote matches the last successful tool call in `working_tool_history` to a node with the same `tool_name`; 2-step disambiguation when several nodes share a tool |
| Guidance model Ψ over the h-hop subgraph | **deterministic render first**: the active node's outgoing edges as `Next / when / do / avoid` lines, capped, appended to `reentry_hint` and `plan_continuation_brief`. A generative Ψ is a later, optional whisper (P5) |
| Guidance as a soft constraint | already the contract: advisory text only; approval policy decides what runs |
| Refiner contrasts failed vs successful trajectories | a fourth `distill.rs` trigger, `ProcedureContrast`, fed one failed and one successful run of the *same* procedure from the run ledger |
| Add/delete edits | `procedure.patch { ops: [add_node, add_edge, delete_edge, delete_node, set_edge_attrs] }` landing as a `procedure_patch` record in `Pending` |
| Held-out validation gate `S_val(G_cand) ≥ S_val(G_prev)` | Philotic has no offline benchmark; the honest analogue is a **live trial window**: after operator approval the candidate version runs for K terminal plan evals and is accepted only if its mean grounded score ≥ the previous version's over its last K runs, else reverted |
| Rejection memory `H_rejected` | rejected patches are never deleted; the last five for a procedure are rendered into the next refiner prompt as *do not re-propose* |
| Expert prior / from-scratch construction | boot seed from existing harness literals (`outcome-reflex`), `procedure.register` for operator/agent authoring, and the distill whisper's YES path emitting a linear Draft procedure from the tool sequence it just saw |

Everything else the paper does — a generative guidance model on every step,
full-graph injection, offline batches over a labelled training set, free-text
node matching — is explicitly **not** borrowed in the first slices (see "Do
not copy").

## Design

### Data model (`ansible-mesh-core/src/graph.rs`)

```rust
/// Node kind: `procedure`. Key: `procedure:{procedure_id}`.
pub struct ProcedureGraphRecord {
    pub procedure_id: String,            // lowercase dotted, e.g. "outcome-reflex", "research.github-digest"
    pub description: String,
    pub skill_name: Option<String>,      // projects with this skill's guidance when set
    pub trigger: Option<String>,         // compiled-in predicate name, e.g. "reports_an_outcome" (P3)
    pub entry: String,                   // node id the backbone starts from
    pub nodes: Vec<ProcedureNode>,       // { id, label, kind: tool|reasoning|state, tool_name: Option }
    pub edges: Vec<ProcedureEdge>,       // { from, to, relation, condition, guidance, pitfalls }
    pub version: u32,
    pub trial_of: Option<String>,        // patch_id while a candidate version is on trial
    pub provenance: ProcedureProvenance, // Repo | Operator | Agent{agent_id} | Refiner
    pub validation_state: SkillValidationState, // reused: Draft/Validated/Registered/Active/Suspended/…
    pub updated_at: u64,
}
pub enum ProcedureRelation { LeadsTo, Triggers, ProvidesInputFor, ConvergesTo }
```

`validate()` is mechanical and runs on register and on every patch: unique
node ids, edges reference known nodes, `entry` exists, ≤ 64 nodes / ≤ 128
edges (the paper's graphs are 7–17 nodes outside BFCL; the cap keeps a
rendered neighbourhood bounded), each text field ≤ 280 chars, and every text
field passes `prompt_guard::detect_prompt_hazard_in` (Self-Improvement L5) —
a `dangerous` verdict rejects, `caution` forces the unconditional approval
tier. `linear_backbone()` walks `LeadsTo` edges from `entry` (first edge in
declaration order, visited set) and is the only projection a plan is ever
seeded from. `locate(last_tool, previous_tool)` exact-matches `tool_name`,
disambiguating by predecessor when several nodes share a tool.

Two more node kinds, both append-only:

- `procedure_run` (`procedure_run:{run_id}`): `procedure_id`, `graph_version`,
  `agent_id`, `session_id`, `turn_id`, `goal` (≤ 200 chars), `tool_sequence`,
  `verdict`, `basis`, `steps_total/verified/done`, `stalls`, `non_atomic`,
  `contradicted`, `guidance_rendered`, `score` (1.0 grounded complete, 0.5
  model-reported complete, 0.0 blocked/stopped), `recorded_at`. Written only
  on a **terminal** plan eval (Complete, Blocked, or budget stop), never on
  `Continue`, and only when a procedure matched.
- `procedure_patch` (`procedure_patch:{patch_id}`): `procedure_id`,
  `base_version`, `ops`, `rationale`, `evidence_run_ids`, `proposed_by`,
  `status: Pending | Trial | Accepted | Rejected`, `trial { started_at,
  baseline: (n, mean), candidate: (n, mean) }`, `rejection_reason`,
  `created_at`, `decided_at`. **Never deleted.**

Storage is the hotel context graph (SQLite `graph_nodes`) through
`GraphDomain`, beside `abstract_skill` — procedures govern agent execution
and are hotel-wide, keyed to skills the same way tool grants are. Not
Memgraph (that is the LifeGraph substrate) and not the project intel-graph
(that is per-machine developer context).

### Projection

Session bind (`aiua/src/service/ipc.rs`, where `effective_skill_guidance` is
composed) adds `effective_procedures`: the full records for every
projectable procedure whose `skill_name` is in the resolved skillset, plus
every standalone procedure with a `trigger`. Records are tiny, so philote
holds them on `SessionState.bindings` and localizes locally — no IPC per
step. `merge_snapshot_bindings` (`philote/src/runtime.rs`) carries them like
skill guidance: prompt-facing, never a tool-assembly rebuild.

### Localized guidance (advisory)

At `reentry_hint` (after tool results) and in `plan_continuation_brief`,
philote finds the active procedure (the plan's `procedure_id` when seeded,
else the bound procedure whose tool-node set best covers the plan's declared
tools, Jaccard ≥ 0.5), locates the active node from the last successful tool
call, and renders its outgoing edges:

```
[Procedure guidance: outcome-reflex @ life.observe]
Next: life.commit — when: the outcome Event is recorded; do: resolve the recalled loop by its exact id, loop_status "resolved", resolution_note citing the Event; avoid: inventing an id, resolving a node that was not recalled.
```

Capped at `MAX_PROCEDURE_GUIDANCE_CHARS = 900`, at most one line per
outgoing edge, pitfalls of the *incoming* edge included when the last call
read as an error. Absent when nothing matches. Kill switch
`PHILOTIC_DISABLE_PROCEDURE_GUIDANCE`. The wording is deliberately the
paper's soft-constraint form; it never adds a tool the toolset lacks and it
never overrides `plan_directive`'s one-step-one-outcome rule.

### Procedure-seeded plans

`seed_outcome_plan` becomes `seed_plan_from_procedure`: the compiled-in
trigger predicate (`plan_eval::reports_an_outcome`) selects the procedure by
its `trigger` field, the plan's steps are the `linear_backbone()` projected
into `PlanStep`s (tool node → `tool_name`, description = node label + the
incoming edge's guidance, operator excerpt appended to the entry step), and
`ActivePlan.procedure_id` is stamped so the run ledger attributes the result.
The two existing literal shapes become one seeded graph with a branch
(`entry → life.recall` when no loop is in context, `entry → life.observe`
when it is; both `ConvergesTo life.commit`). When no procedure is bound the
literal remains as the fallback, so behaviour never regresses on a hotel
that has not seeded.

### Refiner and gate

A fourth `DistillTrigger::ProcedureContrast` fires at a terminal plan eval
when the run's procedure has *both* a success and a failure in its ledger
at the current version (the newest of each, looked up via
`ListProcedureRuns`). The lookaside prompt carries the graph as triplets
with attributes, the failed trajectory (tool sequence, verdict, stalls,
contradicted steps), the successful one, and the last five rejected patches
with their reasons. Legal outputs: `procedure.patch` once, or the exact
reply `PROCEDURE: nothing`. The allowlist gains `procedure.patch` and
`procedure.get`; nothing else. Lane `procedures.refine`, budget 3/day,
kill switch `PHILOTIC_AUTONOMY_DISABLE_PROCEDURES_REFINE`, posture
ProposalOnly.

The gate is two-stage and mechanical:

1. **Approval** — a `Pending` patch is approved by the operator
   (`phil procedure patch approve <id>`, or the approval card). Approval
   applies the ops to a candidate version `v+1` with `trial_of = patch_id`,
   projects it, and moves the patch to `Trial`.
2. **Trial window** — on each `RecordProcedureRun` the hotel checks every
   `Trial` patch for that procedure. After `K` terminal runs on `v+1`
   (`PHILOTIC_PROCEDURE_TRIAL_RUNS`, default 5) it compares the candidate's
   mean score with the previous version's mean over its last `K` runs (or
   whatever exists, minimum one; with no baseline the bar is 0.5). Accept
   when `candidate ≥ baseline` (the paper's `≥`): `v+1` becomes current.
   Otherwise revert to `v`, mark the patch `Rejected` with the two scores
   in `rejection_reason`, and keep it.

Rejection memory is therefore free: the refiner prompt reads `Rejected`
patches for the procedure it is looking at. This generalizes the one
learn/reinforce/suppress loop the stack already runs for role handoffs.

### Construction paths (the paper's five modes, reduced to three)

- **Expert prior** — `seed_procedure_catalog` at hotel boot (fill-only,
  never clobbers a live record with a higher version), starting with
  `outcome-reflex` and `lifegraph.gardening`.
- **Operator/agent authored** — `procedure.register`, gated like
  `skill.register` (orchestrator/management role, DEF-103 risk tier), lands
  `Draft` for agent origin.
- **From trajectories** — the distill whisper's YES path also registers a
  linear Draft procedure (`LeadsTo` chain over the tool sequence it just
  saw, `skill_name` = the Draft skill) so a distilled skill arrives with its
  procedure attached, not just a goal template.

## Slices

| Slice | Content | Size | Verify |
|---|---|---|---|
| P0 `procedure-graph-record` | `ProcedureGraphRecord` / `ProcedureNode` / `ProcedureEdge` / `ProcedureRelation` / `ProcedureProvenance` + `validate()`, `linear_backbone()`, `locate()` in `ansible-mesh-core/src/graph.rs`; node kinds `procedure`, `procedure_run`, `procedure_patch` in `domain/kinds.rs`; `GraphDomain` upsert/get/list; IPC `RegisterProcedure`, `GetProcedure`, `ListProcedures` (+ `ProcedureList` response, inserted **before** `MemoryConfig`); `seed_procedure_catalog` at boot with `outcome-reflex`; `effective_procedures` composed into session bindings and carried by `merge_snapshot_bindings`; `phil procedure list\|show`. | M | test-green: validate rejects dangling edges / oversize / hazard text; backbone walks a branch deterministically; locate disambiguates a shared tool by predecessor; bind projects only projectable procedures |
| P1 `procedure-run-ledger` | `ActivePlan.procedure_id` (serde default); plan→procedure match (stamped id, else Jaccard ≥ 0.5 over tool names); `RecordProcedureRun` IPC sent fire-and-forget from the terminal branches of the plan eval in `turn_loop.rs` (`Settled`, `Stop`), never on `Continue`; hotel persists `procedure_run`; `ListProcedureRuns { procedure_id, version, limit }`; `phil procedure runs <id>`. | S | test-green: a Complete grounded eval records score 1.0 with the tool sequence; a Blocked eval records 0.0; a Continue records nothing; an unmatched plan records nothing |
| P2 `procedure-localized-guidance` | `philote/src/procedures.rs`: active-procedure resolution, `locate` on the last successful call, deterministic `Next/when/do/avoid` render, cap, kill switch; appended by `reentry_hint` and `plan_continuation_brief`; `guidance_rendered` stamped on the run. | S–M | test-green: guidance names only the active node's out-edges; absent with no match; capped; disabled by env; the say-do and plan gates are untouched (existing plan_eval tests stay green) |
| P3 `procedure-seeded-plans` | `seed_plan_from_procedure` replaces the literals in `seed_outcome_plan` when a triggered procedure is bound (branch on recalled target), stamps `procedure_id`; literal fallback retained; `lifegraph.gardening` seeded from the existing gardener SkillDAG shape. | S | test-green: the four existing `seed_outcome_plan` tests pass unchanged against the seeded graph; a hotel with no bound procedure still seeds the literal; `procedure_id` lands on the run |
| P4 `procedure-refiner-gate` | `DistillTrigger::ProcedureContrast`; `procedure.patch` + `procedure.get` tools and IPC (`ProposeProcedurePatch`, `ListProcedurePatches`, `DecideProcedurePatch`); `procedure_patch` records; approval → `Trial` version with `trial_of`; trial-window scoring on `RecordProcedureRun`, accept/revert; rejected patches rendered into the refiner prompt; distill YES path emits a linear Draft procedure; lane `procedures.refine` + kill switch; `phil procedure patches\|approve\|reject`. | L | test-green: contrast fires only with both a success and a failure at the current version; a patch with a dangling edge is refused; approve produces `v+1` on trial; K candidate runs ≥ baseline accept, < baseline revert and keep the patch; a rejected patch appears in the next refiner prompt + watched-live: one real `Pending` patch from a Beacon plan failure on vps-jane, approved, trialled, and decided without a deploy |
| P5 `procedure-generative-guidance` | Optional Ψ: when the deterministic render is empty or the plan has stalled once, a bounded whisper turns the 2-hop subgraph + last three steps into one paragraph of situational guidance, cached per (procedure, node) for the plan's lifetime. Deferred until P2 has a watched-live baseline to compare against. | M | deferred |

Dependencies: P0 → P1 → P2 and P3 (independent of each other) → P4. P5
deferred. L5 `prompt-guard` (built) gates every text field from P0 on.
Autopoiesis A7's "≥3 completed plans with the same sequence" promotion hint
now has a concrete object: three `procedure_run`s at score 1.0 on a `Draft`
procedure is the promotion signal from `Draft → Validated`.

## Do not copy

- **A guidance LLM on every step.** The paper's own limitation: guidance
  tokens rise even when solver steps fall (+33% on GDPval). Philotic renders
  the neighbourhood deterministically and reserves a model for P5.
- **Offline batch validation on a labelled set.** There is none; the live
  trial window under operator approval is the honest analogue and it is
  reversible.
- **Free-text node matching.** Nodes are tool-bound; reasoning/state nodes
  exist in the record but are never localized in P0–P4.
- **Full-graph injection.** The paper shows the localized subgraph beats it
  and cuts tokens ~71%; the cap makes this structural.
- **A fourth memory layer.** Procedures are graph records beside skills, not
  a new store. `CognitiveOutcome::RejectedApproach` keeps flowing to Muninn;
  the procedure-scoped version of that fact is a `Rejected` patch.

## Open questions

- Per-agent versus hotel-wide procedure versions: hotel-wide first (like
  skills). If two agents' ledgers disagree on a trial, the hotel score is
  pooled; revisit if a real conflict appears.
- Trial windows on low-traffic hotels can take days to fill. Acceptable —
  a `Trial` patch is visible in `phil procedure patches` the whole time.

## Disposition

`accepted-current-slice` — **P0–P4 implemented test-green 2026-09-12** on
`codex/procedural-graphs` (core 365, philote 576, hotel procedure tests
green; clippy clean). Two deviations from the slice table, both deliberate:
`guidance_rendered` on a run row is derived from the kill switch plus
attribution rather than a per-turn flag (the `&self` prompt builders cannot
set one), and the `lifegraph.gardening` seed is deferred until the
gardener's live shape is read on vps-jane. Watched-live for P4 (a real
`Pending` patch approved, trialled, and decided on vps-jane) is the open
gate. Status truth lives in [ARCHITECTURE_STATUS.md](ARCHITECTURE_STATUS.md);
execution order in [docs/task.md](../task.md) → `New Project: Procedural
Graphs`.
