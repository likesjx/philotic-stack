---
title: Reflexive Life Graph — Skills That Own Their Structure
doc_type: proposal
domain: memory-context
status: proposed
disposition: proposed
verification_level: none
last_updated: 2026-09-15
tags:
  - lifegraph
  - skills
  - ontology
  - invariants
  - cypher
  - gardener
  - procedural-graphs
  - reflex
related_docs:
  - GRAPH_DOORS_AND_LIFE_CORE_PROPOSAL.md
  - LIFE_GRAPH_OS_PROPOSAL.md
  - LIFE_GRAPH_ACTIVE_PROPOSAL.md
  - PROCEDURAL_GRAPHS_PROPOSAL.md
  - SKILL_GOVERNANCE_HARDENING_PROPOSAL.md
  - SKILL_LIFECYCLE_PROPOSAL.md
  - life-graph/LIFE_GRAPH_SCHEMA.md
  - ARCHITECTURE_STATUS.md
  - SEAM_REGISTRY.md
task_refs:
  - docs/task.md
proposal_id: reflexive-life-graph
implements: []
implemented_by: []
active_seams:
  - lifegraph-typed-properties
  - skill-ontology-binding
  - skill-invariants
  - skill-procedure-binding
  - lifegraph-reflex-triggers
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
---

# Reflexive Life Graph — Skills That Own Their Structure

**Operator direction (2026-09-15):** the LifeGraph is where the operator's life gets
structured to a deep, granular level, and it must be *reflexive*: the philotes keep it
current and correct themselves, and the skills they run carry the structure — and where
useful the Cypher — that keeps it so. This proposal names what "reflexive" requires
that the stack does not yet have, and sequences it on top of the Graph Doors and
Procedural Graphs work.

## Why now

Two days of watched-live work on bjork's repertoire (2026-09-14/15) showed the graph
staying correct only because the operator caught each drift by hand:

| What happened | Why the skill could not prevent it |
|---|---|
| Eight `MusicSection` nodes were hung `RELATES_TO` open loops, not `HAS_SECTION` from their piece (09-14 14:43) | the skill was a prose goal template; the ontology patch that defined `HAS_SECTION` was not bound to it |
| The Mendelssohn piece was created twice, first as `life:project:…`, then as `life:creative_work:…` (09-15 14:26) | no idempotent id rule (opus number) and no label the skill owns |
| Key, tempo and difficulty were requested three times and exist only as prose in `claim_summary` | `EvidencePacket` has no typed-property slot; `life.observe` writes provenance fields only ([provider.rs](../../crates/data-memorygraphrag/src/provider.rs) `observe` params), so [LIFE_GRAPH_SCHEMA.md](life-graph/LIFE_GRAPH_SCHEMA.md)'s per-label property tables are aspiration, not contract |
| Bjork spawned a worker to run the gardener on a new piece; the worker received the goal and timed out silently (09-15 14:24) | `philote-worker` is a single model call with no tool loop (DEF-129) — nothing reflexive can run off-thread |
| The rule "use the gardener whenever a piece is mentioned" was registered twice as prose (DEF-133) | triggers are prompt text, not structure |

The gardener slices (`life.audit` / `life.tidy`, the audit → seeded-tidy reflex, write
receipts, the closing-audit fence) proved the *maintenance* half of reflexivity works:
graph science finds duplicates, orphans and islands, and a philote applies governed
actions. What is missing is the *authoring* half: a skill that writes to the graph
must know which labels, edges and properties it owns, must be able to state the
invariants that hold when it has done its job, and must run as ordered, idempotent
steps — so that a retry cannot mint a duplicate and a section cannot land on the wrong
parent.

## Core recommendation

A skill that writes to the LifeGraph is a **graph-bound procedure**, not a paragraph.
`AbstractSkillRecord` gains four structured fields, each validated at `skill.register`
and each consumed by an existing organ:

1. **`ontology_scope`** — the labels, relationship types and typed properties the skill
   owns (`CreativeWork{key, tempo, difficulty:0-100, accuracy:0-100}`, `MusicSection{
   measure_span, focus}`, `CreativeWork -HAS_SECTION-> MusicSection`, `Event
   -PRACTICES-> MusicSection`). Validated against the compiled ontology plus applied
   `SchemaPatch` records; a skill cannot register a scope the ontology does not know.
   Rendered into the skill's projection so the model sees the exact vocabulary.
2. **`invariants`** — checks that hold when the skill has done its work. Two tiers:
   *declarative* (`every CreativeWork has ≥1 HAS_SECTION`, `CreativeWork unique by
   catalogue id`, `MusicSection.measure_span present`) which the runner compiles to
   parameterised read-only Cypher itself; and *raw Cypher* invariants, which only ship
   once the Cypher wall (Graph Doors G1) classifies them as read-only. Violations return
   in the `life.audit` `suggested_actions` shape, so the existing gardening reflex seeds
   the tidy plan without new plumbing.
3. **`procedure`** — the skill's ordered steps as a `ProcedureGraphRecord` (Procedural
   Graphs P0): `life.list` existing → observe the piece with typed properties → observe
   sections → link → run invariants. Node ids are derived from canonical keys (opus
   number, ISO date) so a retry converges instead of duplicating.
4. **`triggers`** — structural selection: label mentions and entity kinds that project
   the skill (`piece_mentioned`, `practice_reported`), replacing prose rules.

Underneath, two prerequisites: the observe contract gains a **typed `properties`
map** validated against the ontology and written onto the node, and the worker gains a
**tool loop** (DEF-129) so a skill can run on a cron or a whisper, unattended.

The reflex loop this yields: *operator mentions a piece → trigger projects the skill →
the procedure writes typed nodes under the skill's scope → the invariants run → any
violation seeds a tidy plan → the receipt reports what changed and the health delta.*

## What already exists, and what this builds on

- **Graph Doors** ([GRAPH_DOORS_AND_LIFE_CORE_PROPOSAL.md](GRAPH_DOORS_AND_LIFE_CORE_PROPOSAL.md)):
  G6 `skill-guidance-cypher` gives a skill a *guidance* block that may carry Cypher for
  the model to use. This proposal is the runner-side complement: invariants the
  **runner** executes, not advice the model may follow. Raw-Cypher invariants depend on
  G1 (`cypher-classifier-wall`); declarative invariants do not.
- **Procedural Graphs** ([PROCEDURAL_GRAPHS_PROPOSAL.md](PROCEDURAL_GRAPHS_PROPOSAL.md)):
  `procedure.register` and `ProcedureGraphRecord` exist (P0–P4 test-green). Slice R3 binds
  a procedure to a skill's scope instead of learning it from distill alone.
- **Gardener** (`life.audit`, `life.tidy`, seeded tidy plans, write receipts, closing-audit
  fence — PRs #506, #511, #519): the consumer of invariant violations. No change to its
  action kinds (`retire_duplicate`, `link`, `resolve`, `retire`).
- **Ontology patches** (`life.patch.propose/apply`, `SchemaPatch`): the source
  `ontology_scope` is validated against. The 2026-09-14 `patch:music_repertoire_ontology_v3`
  (`MusicSection`, `HAS_SECTION`, `FOCUSES_ON`) is the first scope a skill will claim.
- **Skill governance** (risk-tiered `skill.register`, audits): `ontology_scope` and
  `invariants` are new risk inputs — a skill claiming a scope it does not own, or an
  invariant that is not read-only, is refused at registration.

## Design

### R1 `lifegraph-typed-properties`

`EvidencePacket.properties: BTreeMap<String, Value>` (default empty). `life.observe`
and `life.observe.batch` validate each key against the ontology's property table for
the claim's label (name, type, range: `difficulty` is an integer 0–100, `key` a string
from the key vocabulary, `tempo` a string or BPM integer) and write the map onto the
node alongside the provenance fields. Unknown keys are rejected with the same
`contract_invalid` shape the batch already uses, naming the label and the allowed
keys. Re-observing a node updates the properties (the DEF-123 `claim_summary` update
path is the model). `life.list` and `life.recall` return them. The schema doc's
per-label tables become the contract they already claim to be.

### R2 `skill-ontology-binding`

`AbstractSkillRecord.ontology_scope { labels, rel_types, properties }`. `skill.register`
rejects a scope whose labels, rel types or properties are unknown to the ontology after
applied patches (`SKILL_SCOPE_UNKNOWN`, naming the offender and the nearest known
name — the `Project`/`CreativeWork` slip). The scope renders into the skill's
projection as a fixed vocabulary block. `life.observe` called while a scoped skill is
active warns when the claim's label is outside the scope (not refused: the gardener
must be free to touch anything).

### R3 `skill-procedure-binding` and idempotent ids

`AbstractSkillRecord.procedure_id` points at a `ProcedureGraphRecord`; `skill.register`
accepts the procedure inline (the `procedure.register` args) and registers both. Step
nodes carry the scope's labels; the harness seeds the procedure as the active plan when
the skill projects (the seeded-tidy-plan mechanism, generalised). Id derivation: a
procedure step may declare `id_from: [composer, opus]` and the runner mints
`life:creative_work:<slug>` deterministically, so the second attempt MERGEs the first.

### R4 `skill-invariants`

`AbstractSkillRecord.invariants: Vec<Invariant>` where
`Invariant = Declarative { rule, params } | Cypher { statement, expect }`. The runner
gains `life.invariants` (read-only): compiles declarative rules to parameterised
Cypher with a forced `LIMIT`, runs Cypher invariants only when `cypher-guard`
classifies them read-only, and returns `{ violations: [{ rule, node_ids, suggested_action }] }`
in the `life.audit` action shape. The harness runs the skill's invariants as the
closing step of its procedure (the closing-audit fence already exists for the gardener)
and feeds violations to the gardening reflex. Declarative rules ship first; the Cypher
tier is gated on Graph Doors G1.

### R5 `lifegraph-reflex-triggers`

`AbstractSkillRecord.triggers: Vec<Trigger>` — `label_mentioned(CreativeWork)`,
`entity_kind(piece)`, `event_reported(practice)`. The turn's auto-recall already
classifies the operator's message against the ontology; a matching trigger projects
the skill for that turn and records `trigger_fired` in the turn events. The prose rule
bjork registered twice becomes one structural trigger; DEF-133 (rule re-proposal after
approval) loses its motivation.

### Prerequisite: worker tool loop (DEF-129)

None of the above runs unattended until `philote-worker` can execute tools. The
smallest version: the worker reuses the philote turn loop with tools projected from
the delegation's `allowed_tools`, the skill's scope, and an iteration budget; its
completion hook carries the tool ledger and the invariant result, not prose. This is
tracked as its own seam under Data-Driven Tool Grants; R1–R5 are designed so a
philote can run them inline in orchestrator posture until it lands.

## Slices

| Slice | Content | Size | Verify |
|---|---|---|---|
| R1 `lifegraph-typed-properties` | `EvidencePacket.properties`, ontology validation per label, written on the node, returned by `life.list`/`life.recall`; `CreativeWork{key,tempo,difficulty,accuracy}` and `MusicSection{measure_span,focus}` in the schema patch | S–M | test-green contract + provider; watched-live: bjork records difficulty 55 on the Handel, `life.list` returns it typed |
| R2 `skill-ontology-binding` | record field, `skill.register` validation, projection block, out-of-scope warning | S | test-green; watched-live: a `Project` claim from the repertoire skill is warned, `CreativeWork` is not |
| R3 `skill-procedure-binding` | `procedure_id` on the record, inline registration, seeded plan on projection, `id_from` deterministic ids | M | test-green; watched-live: a new piece named twice in one day yields one node |
| R4 `skill-invariants` | `Invariant` record, `life.invariants` runner tool (declarative tier), closing step, violations → gardening reflex | M | test-green; watched-live: a section written without a parent is reported and tidied in the same turn |
| R4b `skill-invariants-cypher` | raw Cypher tier behind `cypher-guard` | S | after Graph Doors G1 |
| R5 `lifegraph-reflex-triggers` | trigger records, auto-recall classification hook, `trigger_fired` event | S–M | watched-live: "I'm starting the Mendelssohn" projects the repertoire skill without a rule |

Order: R1 → R2 → R4 (declarative) → R3 → R5 → R4b. R1 first because every other slice
writes typed values; R4 before R3 because invariants are what make the procedure's
closing step meaningful.

## Do not build

- Operator-run Cypher against the LifeGraph, for any reason but a test fixture. The
  runner runs a skill's invariants; a person never runs a patch (operator direction,
  2026-09-12).
- A per-skill free-form property bag. Properties are validated against the ontology or
  refused; prose in `claim_summary` stays prose.
- Invariants that mutate. `life.invariants` is read-only; repair goes through
  `life.tidy` and the gardening reflex so every change carries a receipt.
- A second procedure language. Procedures are `ProcedureGraphRecord`s; the skill
  binds one, it does not embed another.

## Disposition

Proposed 2026-09-15. Slice 1 = R1 (typed properties) — the smallest change that turns
"difficulty 0–100" from a sentence into a number the operator can cross-reference
against practice. Records and audit live in the hotel context graph; the proposal is
indexed from the main checkout after merge.
