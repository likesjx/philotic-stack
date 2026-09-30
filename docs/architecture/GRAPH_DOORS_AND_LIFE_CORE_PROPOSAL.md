---
title: Graph Doors and the Life Graph as Core — Tiered Memgraph Exposure Behind One Cypher Wall
doc_type: proposal
domain: memory-context
status: proposed
disposition: proposed
last_updated: 2026-09-05
tags:
  - memgraph
  - cypher
  - lifegraph
  - graph-datasource
  - safety-floor
  - mage
  - skills
related_docs:
  - LIFE_GRAPH_OS_PROPOSAL.md
  - GRAPH_DATASOURCE_PROPOSAL.md
  - GRAPH_LAYER_UNIFICATION_PROPOSAL.md
  - SELF_IMPROVEMENT_LOOP_PROPOSAL.md
  - AUTOPOIESIS_PROPOSAL.md
  - MEMORY_TRANSPARENCY_PROPOSAL.md
  - PERIMETER_EGRESS_CONTROL_PROPOSAL.md
  - ARCHITECTURE_STATUS.md
task_refs:
  - docs/task.md
proposal_id: graph-doors-life-core
implements: []
implemented_by: []
active_seams:
  - cypher-classifier-wall
  - graph-registry-containers
  - graph-door-query
  - graph-door-mutate
  - graph-door-analyze
  - graph-door-admin
  - life-core-layer
  - skill-guidance-cypher
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
---

# Graph Doors and the Life Graph as Core

**Operator intent (2026-09-05, paraphrased from the conversation):** expose
Memgraph safely and in tiers; expose 100% of Cypher and the MAGE algorithms,
but not all in one place; every mutation goes through a writing tool; there is
an admin tool; and split the stack into low-level Memgraph toolsets with the
LifeGraph toolsets on top, so the same Memgraph can hold several graphs with
Life as the core one. Partitioning decision: **separate containers per graph
class.**

## Why now

Three facts from the code and the live hotels on 2026-09-04/05:

1. **`graph.query` on mac-jane does not reach the LifeGraph.** The hotel has
   no `PHILOTIC_MEMGRAPH_URI` and no graph-datasource process; a philote's
   Cypher runs through the SQLite compatibility bridge over its own agent
   partition. The only road to the shared LifeGraph is the `life.*` tool
   family, routed over the mesh to the runner on vps-jane. Bjork's first
   distilled skill (`music.weekly-practice-review`, Draft, 2026-09-04) encoded
   a `graph.query` step for exactly this reason — the model reached for the
   only Cypher door it had and it was the wrong graph.
2. **Partitioning is logical only.** The Memgraph provider records
   `graph_id` on each task and logs "partition create is logical; physical
   database is Bolt endpoint" (`graph-datasource/src/memgraph_provider.rs:122`).
   Nothing rewrites queries or separates data; the partition is a label on the
   request.
3. **Skills cannot carry queries.** The guidance a philote sees per skill is
   `name — description` (`aiua/src/service/ipc.rs`, `push_guidance`), capped at
   eight entries; the goal template is used only when a skill delegates to a
   worker. There is nowhere in a skill record for a paragraph of doctrine or a
   Cypher statement, so "more Cypher in the skills" has no home today.

The LifeGraph OS proposal made Life the operator's world-model; the Cypher-First
Graph Datasource project made Memgraph the engine; the Graph Layer Unification
proposal asked for one adapter under every graph. This proposal is the missing
exposure layer: how agents get *all* of Cypher without any of it being unsafe.

## Core recommendation

**Two layers, four doors, one wall.**

```
                 ┌────────────── Layer 2: LifeGraph (ontology-aware) ──────────────┐
                 │ life.observe  life.commit  life.resolve  life.patch.*  life.recall │
                 │ life.list     life.query   life.analyze   (steward sweeps)          │
                 │        pins graph=life · injects the Life ontology · proposed→confirmed writes │
                 └────────────────────────────┬───────────────────────────────────┘
                                              │ calls
                 ┌────────────── Layer 1: Memgraph doors (graph-addressed) ─────────┐
                 │ graph.query   graph.analyze   graph.mutate   graph.admin           │
                 │   read          algorithms      writes         DDL/ops             │
                 └────────────────────────────┬───────────────────────────────────┘
                                              │ every statement, every door
                 ┌────────────── The wall: cypher-guard (compiled in) ──────────────┐
                 │ tokenizer → statement class {read, algorithm, mutation, admin}     │
                 │ destructive-shape floor · bounds · provenance stamp · audit        │
                 └────────────────────────────┬───────────────────────────────────┘
                                              │ Bolt, endpoint chosen by graph id
   philotic-memgraph-life   philotic-memgraph-agents   philotic-memgraph-sandbox   (…more)
```

- A **door** is a tool. It admits exactly one statement class and takes a
  `graph` argument (Layer 1) or pins it (Layer 2). A door is a convenience for
  toolset projection and grants; it is not the security boundary.
- The **wall** is `cypher-guard`, a compiled-in crate in the exec-guard /
  prompt-guard family. The runner re-classifies every statement regardless of
  which door it came through and refuses class mismatches. No policy record,
  grant, `auto_approve_all`, or "trust for session" can widen it.
- A **graph** is a registry record: id, container endpoint, owner, ontology
  reference, visibility, and the per-role door grants. `life` is seeded and
  always present; `agent:<id>` partitions become real graphs in their own
  container; new graphs are records, not code.
- **Life is core**: seeded ontology, the only graph shared across hotels by
  default, and the strictest mutation posture. Layer 2 exists so ordinary
  agents never need Layer 1 to work with Life.

## Layer 1 — the doors

| Door | Statement class admitted | Bounds and stamps | Default grant |
|---|---|---|---|
| `graph.query` | `MATCH`, `OPTIONAL MATCH`, `UNWIND`, `WITH`, `RETURN`, `ORDER BY`, `SKIP`, `LIMIT`, `CALL` of read-only procedures (`mg.procedures`, `mg.functions`, `SHOW INDEX INFO`, `SHOW STORAGE INFO`, `SHOW CONSTRAINT INFO`, `EXPLAIN`, `PROFILE`) | row cap (default 200), byte cap (default 64 KB, the MCP upstream `max_response_bytes` precedent), per-query timeout (default 10 s), forced `LIMIT` when absent | every role, every graph its role can see |
| `graph.analyze` | MAGE and algo procedures that do not write: `pagerank.get`, `betweenness_centrality.get`, `community_detection.get`, `node_similarity.*`, `kmeans.get_clusters`, `nxalg.*` read variants, `algo.*` path finders | same caps plus a daily budget per agent via autonomy lane `graph.analyze`; results return to the caller and never persist unless the caller then uses `graph.mutate` | every role, budgeted |
| `graph.mutate` | `CREATE`, `MERGE`, `SET`, `REMOVE`, `DELETE`, `DETACH DELETE`, MAGE procedures that write | dry run first (`EXPLAIN` + affected-node/edge count from a preceding read); every node and edge written is stamped `observed_by`, `graph_id`, `written_at`, `validation_state` (default `proposed`); audit record with a reversal hint (the inverse statement or the pre-image of touched properties); destructive-shape floor refuses `MATCH (n) DELETE n`, `DETACH DELETE` without an id or `LIMIT`, `REMOVE` of provenance properties, and unbounded `SET` | autonomy lane `graph.mutate`, ConfirmFirst for everyone including the orchestrator until earned |
| `graph.admin` | `CREATE`/`DROP INDEX`, constraints, triggers, `FREE MEMORY`, `DUMP DATABASE`, `CALL mg.load_all()`, users and privileges, storage mode | unconditional live operator approval on every call (the `skill_register`-style unconditional gate), audited, refused outright for any graph whose registry record says `admin: operator-only` and the caller is not orchestrator/management | orchestrator and management only |

Introspection that is read-only (`SHOW INDEX INFO`, `SHOW STORAGE INFO`) lives
on the query door so agents can plan without admin rights.

## The wall — `cypher-guard`

A tokenizer, not a regex. It produces a statement list with, per statement:
the clause set, called procedure names, whether a `CALL … YIELD` feeds a write,
presence of `LIMIT`, presence of an id-equality predicate on every deleted
pattern, and the class:

- `Read` — only read clauses and read-only procedures.
- `Algorithm` — a read that calls a procedure on the analytics allowlist.
- `Mutation` — any write clause or a writing procedure.
- `Admin` — DDL, ops, auth, module load, `USE`/`CREATE DATABASE`.
- `Refused` — the destructive-shape floor (unbounded delete, provenance
  stripping, `DROP DATABASE`, anything the tokenizer cannot parse).

Rules: a door admits exactly its class; the runner classifies again server-side;
`Refused` is not overridable at any tier; unparseable input is refused, never
"probably read". Fixture corpus in the exec-guard/prompt-guard style, at least
one hundred statements including MAGE calls in each class and the classic
disguises (`WITH * CALL {…} DELETE`, `FOREACH … SET`, a write hidden after
`UNION`). Prompt-guard scans any Cypher that lives in a skill before it is
registered (L5), so a skill cannot smuggle a mutation into a query door
either.

## The registry — graphs as records, containers as endpoints

`graph_registry` node kind in the hotel graph, mesh-synced like cron jobs:

| Field | Meaning |
|---|---|
| `graph_id` | `life`, `agents`, `sandbox`, `project:<slug>`, … |
| `endpoint` | Bolt URI of the container serving it (`bolt://100.64.212.8:7687` for life) |
| `container` | `philotic-memgraph-life`, `philotic-memgraph-agents`, `philotic-memgraph-sandbox` |
| `owner` | operator, or an agent id for a private graph |
| `ontology_ref` | for Life, the LifeGraph ontology document; optional elsewhere |
| `visibility` | `shared`, `hotel`, `owner` |
| `door_grants` | per role tier: which of the four doors may open on this graph |
| `mutate_posture` | starting posture of the `graph.mutate` lane on this graph |
| `mage` | whether the container ships MAGE and which procedure families are allowed on `graph.analyze` |

**Why containers.** Memgraph's real multi-database isolation is an Enterprise
feature, and label-partition-plus-query-rewrite is exactly the "logical
partition" that currently enforces nothing. One container per graph class gives
hard isolation, an obvious blast radius, and independent memory limits: an
analytics run on `agents` cannot stall Life. The registry maps id → endpoint, so
moving a graph onto a multi-database instance later is a registry edit, not a
tool change. Compose on vps-jane grows from one `philotic-memgraph` service to
`philotic-memgraph-life` (the existing container, renamed, MAGE image),
`philotic-memgraph-agents`, and `philotic-memgraph-sandbox`, each with its own
persistent volume and backup, Tailscale-bound like today.

**Where today's partitions go.** The current per-agent `graph_id` partitions
(`agent:<id>`) become label-scoped graphs inside the `agents` container, and
`graph.query` defaults `graph` to `agent:<self>` so nothing an agent does today
breaks. The SQLite transpiler bridge on hotels without a Memgraph route stays
as the explicit compatibility path the Cypher-First project already names.

## Layer 2 — Life as the core graph

Layer 2 tools are thin, ontology-aware wrappers over Layer 1 with `graph=life`
pinned:

- `life.query` = `graph.query` on `life` with the Life ontology (labels,
  relationship types, validation states, provenance properties) injected into
  the tool description and skill guidance, and the named maintenance queries
  (`past_dated_events`, `aging_loops_oldest_first`, …) republished as
  Cypher the model can read.
- `life.analyze` = `graph.analyze` on `life` (community detection over roles,
  loops, and people; centrality of open loops).
- `life.observe`, `life.commit`, `life.resolve`, `life.patch.*` re-base on
  `graph.mutate` and inherit its dry run, provenance stamping, audit, and
  reversal hint — while keeping the proposed → confirmed contract that makes a
  Life write safe for an ordinary agent without ever seeing Cypher.
- `life.recall`, `life.list`, and the steward sweeps stay as they are; they
  are the retrieval half of the LifeGraph OS proposal and are untouched by the
  door model beyond running through the same wall.

Life is "core" in three enforceable ways: the registry record is seeded at
hotel boot and cannot be dropped through `graph.admin`; it is the only graph
with `visibility: shared` by default; and its `graph.mutate` lane starts at
ConfirmFirst for every role, orchestrator included, and earns autonomy through
the trust ledger like any other lane.

## Cypher in skills

`AbstractSkillRecord` gains `guidance: Option<String>` (bounded, default cap
2,400 chars per skill, 8 skills per turn as today) rendered into the
`[Skill guidance]` block when the skill projects. This is where a skill keeps
its queries — `music.weekly-practice-review` carries the `life.query` Cypher for
"practice Events in the last seven days, grouped by instrument, with the goal
they advance"; a project skill carries its `graph.query` against
`project:<slug>`. `skill.register` and the Self-Improvement Loop's `skill.patch`
(L3) accept the field; prompt-guard scans it; the distill whisper (L1) is
told it may fill it. The skill-authoring doctrine adds one line: *a skill that
reads a graph names its door and carries its query.*

## Slices

| Slice | Content | Size | Verify |
|---|---|---|---|
| G0 `graph-registry-containers` | `graph_registry` node kind + seed of `life`/`agents`/`sandbox`; compose on vps-jane split into three Memgraph services (life keeps the existing volume, MAGE image); provider resolves endpoint by graph id; `PHILOTIC_MEMGRAPH_URI` becomes the `life` endpoint default. `phil graph registry` lists them. | M | smoke-green: `life` answers on its endpoint after the split with the same node count (509 on 2026-09-05); `agents` and `sandbox` answer empty |
| G1 `cypher-classifier-wall` | `crates/cypher-guard`: tokenizer, statement classes, destructive-shape floor, ≥100-statement corpus. Wired into the runner ahead of every Bolt call. | M | test-green corpus; runner refuses a mutation sent through the query path in a unit test |
| G2 `graph-door-query` + `life-core-layer` (read half) | `graph.query` rebuilt on the wall with bounds and a forced `LIMIT`; `life.query` wrapper with ontology injection; Bjork's `music.practice-log` (uses `life.observe`) and a rewritten `music.weekly-practice-review` (uses `life.query`) assigned to her `virtuosa` and `orchestrator` roles; Beacon's rule reduced to "whisper Bjork's virtuosa". | M | watched-live: operator tells Beacon about a session → whisper → Bjork records an Event visible in Memgraph → her review's `life.query` returns it |
| G3 `graph-door-mutate` | `graph.mutate` with dry run, provenance stamps, audit + reversal hint, destructive floor; `life.observe`/`commit`/`resolve`/`patch` re-based on it; lane `graph.mutate` ConfirmFirst per graph. | M–L | test-green (dry-run counts, stamps, refused shapes) + watched-live: one confirmed mutation with a reversal hint the operator applies |
| G4 `graph-door-analyze` | `graph.analyze` with the MAGE allowlist, lane `graph.analyze` budget, MAGE image on the life container; `life.analyze`. | S–M | smoke-green: community detection over Life returns bounded results within the timeout |
| G5 `graph-door-admin` | `graph.admin` under the unconditional operator gate: indexes, constraints, triggers, dump, free memory, module load, users where the edition allows. | S | watched-live: operator approves one index creation, refuses one drop |
| G6 `skill-guidance-cypher` | `AbstractSkillRecord.guidance` + `skill.register`/`skill.patch` args + rendering + prompt-guard scan; distill brief may fill it. | S–M | test-green + the rewritten practice review carries its Cypher in guidance |
| G7 runner unification | life-graph-runner consumes the Layer 1 provider instead of its own Bolt path (Graph Layer Unification direction). | M | test-green; no behaviour change in `life.recall` |

Order: G0 → G1 → G2 (this is what gives Bjork her practice skills on the right
graph) → G3 → G6 → G4 → G5 → G7. G2 before G3 is deliberate: the read door plus
`life.observe` as it exists today already lets the practice loop close; the
raw mutation door is a power tool that waits for the wall to be proven live.

## Answers to the design questions raised on 2026-09-05

- **Read scope:** reads on `life` see the whole shared graph; private subgraphs
  come later as a `visibility` label, not as filtering now.
- **`graph.analyze` runs live, not on a projection:** container isolation
  already bounds the blast radius, and the lane budget bounds the rate.
- **`graph.mutate` starts ConfirmFirst for everyone**, orchestrator included;
  autonomy is earned per graph through the existing trust ledger.
- **No single `graph.cypher` with a mode flag.** Separate doors are what let a
  toolset profile grant reads to every role while keeping mutation and admin
  narrow, and what stop a skill granted the query door from being talked into
  a write.

## Do not build

- Query rewriting to fake partitions inside one database.
- A Memgraph MCP sidecar as the agent path (it bypasses the wall; if ever
  wanted for operator tooling it sits behind `graph.admin`).
- Unbounded reads "because it is only a read": the byte cap is what keeps a
  `MATCH (n) RETURN n` from becoming the next 236 KB request.

## Disposition

`proposed` — awaiting operator review. Filed 2026-09-05 from the practice-tracking
conversation: Beacon claimed to log a practice session without a tool call,
Bjork's distilled review reached for the wrong graph, and both trace back to
the same missing layer.
