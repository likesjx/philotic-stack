---
title: Life Graph Schema
doc_type: reference
domain: memory-context
status: active
last_updated: 2026-07-24
tags:
- life-graph
- schema
- cypher
- memgraph
- vocabulary
related_docs:
- ../LIFE_GRAPH_OS_PROPOSAL.md
- ../SEAM_REGISTRY.md
seam: life-graph-schema
source_of_truth_targets:
- SEAM_REGISTRY.md
- docs/task.md
---

# Life Graph Schema

Canonical node/edge vocabulary for the Life Graph OS. All Cypher migrations, toolset routes, context projection rules, and retrieval strategies derive from this file.

## Substrate

- **Database**: Memgraph 3.10.1+ via `graph-datasource` (memgraph-cypher provider)
- **Bolt endpoint**: `100.64.212.8:7687` (vps-jane Tailscale)
- **Database name**: default (single-database deployment; logical partition via node labels)
- **Migrations**: `migrations/V001__life_graph_schema.cypher` through
  `migrations/V006__creative_learning_flywheel.cypher`

The `graph-datasource` crate remains generic. It does not know that a graph contains life data. Life Graph OS semantics live in `data-memorygraphrag` and in this schema vocabulary.

## Provenance Envelope

Every node and every agent-written or inferred edge **must** carry these properties:

| Property | Type | Description |
|---|---|---|
| `source_membrane` | `string` | Transport the evidence arrived over (e.g. `membrane:telegram`, `hotel:mbp-jane`) |
| `observed_by` | `string` | Canonical agent identity that made the observation (e.g. `agent-astrid-01`). `agent:unknown` for legacy writes (V005+) |
| `observed_role` | `string \| null` | Active role of the observing agent at write time (e.g. `chief_of_staff`), if any (V005+) |
| `provenance` | `string` | Claim origin: `user_input`, `transcript`, `calendar`, `health_data`, `agent_inferred`, `operator_confirmed`, `muninn_engram` |
| `origin_engram_id` | `string \| null` | Muninn engram ID when the observation's source was a `muninn_engram` source ref (source_kind `MuninnEngram`); `null` otherwise. Written at observe time so promotion stays auditable |
| `origin_trust` | `float \| null` | Reliability score (0.0–1.0) of that Muninn source ref at write time; `null` when no Muninn origin |
| `confidence` | `float` | 0.0–1.0. Inferred facts start low; rise with evidence and operator confirmation |
| `validation_state` | `string` | `inferred` \| `proposed` \| `confirmed` \| `retired` \| `conflicted` |
| `observed_at` | `string` | ISO 8601 timestamp when first observed |
| `last_confirmed_at` | `string \| null` | ISO 8601 timestamp of last operator or strong-evidence confirmation |
| `capture_kind` | `string \| null` | Quick-capture kind: `inbox`, `question`, `idea`, `source`, `experiment`, `artifact`, or `learning` |
| `creative_status` | `string \| null` | Lightweight creative lifecycle state, initially `inbox` or `captured` |
| `inbox_state` | `string \| null` | `unclassified` for deferred-classification captures |
| `pilot_domain` | `string \| null` | Optional single-domain pilot scope used by bounded briefs and reviews |

Operator-created nodes may omit provenance fields; they are required on agent-written records.

`life.capture` expands a minimal `{content, kind?, pilot_domain?, source_id?,
edges?}` payload into the same governed `life.observe` path. Captures are
always `proposed`; an omitted kind becomes a `Signal` with
`capture_kind=inbox` and `inbox_state=unclassified`.

`life.observe` also accepts an optional `edges[]` field (`{rel_type,
target_id}`) MERGE'd idempotently with the node write. `rel_type` must be one
of the living-cycle set `OWNS | SHAPES | SETS | SPAWNS | RELATES_TO |
INSPIRES | INFORMS | TESTED_BY | PRODUCES | EXPRESSES | REFINES |
SHARED_WITH | SCOPED_TO` (unknown rel_types are rejected before the node
write); a `target_id` matching no existing node creates nothing and is
reported as `target_missing` in the response envelope. Domain zoning Role
nodes (`domain_slug`, `steward_agent`) are seeded by
`migrations/V005__domain_role_zoning_seed.cypher`.

### Muninn Promotion Contract (seam: `lifegraph-muninn-promotion`)

How a Muninn continuity memory becomes Life Graph truth:

1. **Entry — origin preserved, never laundered.** A `life.observe` whose evidence carries a `MuninnEngram` source ref writes the node with `provenance = "muninn_engram"`, `origin_engram_id` = the engram's ID, and `origin_trust` = the source ref's reliability score. The Muninn origin is never collapsed into `agent_inferred`. The first `MuninnEngram` source ref wins when several are present; when a non-Muninn source ref comes first, it still drives `provenance`/`source_membrane` (transport truth) while the Muninn origin fields are preserved alongside.
2. **Proposed, not confirmed.** Muninn-origin nodes enter as `validation_state = proposed` like any other agent observation. Muninn is a continuity authority, not a Life Graph truth authority — a high-trust engram does not skip the gate.
3. **Retrieval bias, bounded.** Ranking gives unconfirmed nodes with `origin_trust >= 0.7` a small confirmation-term lift (`+0.15` on the confirmation axis, capped at 1.0) so trusted continuity surfaces more readily — but it can never outrank operator confirmation on that axis. Nodes written before this contract (no `origin_trust` property) rank exactly as before.
4. **Hardening — the existing `life.commit` gate.** Promotion to `validation_state = confirmed` happens only through the existing `life.commit` confirmation path (operator confirmation or strong-evidence adjudication). No automated Muninn-to-confirmed promotion exists; building one is a deliberate future decision, not an implicit behaviour of this contract.

`origin_engram_id` keeps the promotion auditable end-to-end: a confirmed fact can always be traced back to the engram that seeded it (and disputed via the conflict-handoff path if Muninn and the graph later disagree).

## Embedding Metadata

Nodes that participate in semantic retrieval carry these additional properties alongside the `embedding` vector:

| Property | Type | Description |
|---|---|---|
| `embedding` | `list<float>` | The vector. Indexed by the node's vector index. |
| `embedding_model` | `string` | Model ID used to produce the vector (current local baseline: `Xenova/all-mpnet-base-v2`) |
| `embedding_model_gen` | `int` | Generation counter — increment when the model changes to trigger re-embedding |
| `embedding_dims` | `int` | Dimension. Must match the vector index config (`768` baseline) |
| `embedding_hash` | `string` | SHA-256 hex of the raw embedding bytes |
| `embedding_updated_at` | `string` | ISO 8601 timestamp of last embedding write |
| `embedding_source_text_hash` | `string` | SHA-256 hex of the source text that was embedded |
| `embedding_space` | `string` | Logical space name (see Vector Spaces below) |

Never mix models or dimensions inside one `embedding_space`. When `embedding_model_gen` changes, schedule a re-embedding job before relying on retrieval results from that space.

---

## Node Types

### Baseline fields (all nodes)

`id: string` (unique per label), `created_at: string` (ISO 8601).

---

### Person

People the operator has meaningful relationships with.

| Property | Notes |
|---|---|
| `name` | Display name |
| `aliases` | `list<string>` — nicknames, usernames, roles used in identifying text |
| `notes` | Free text |

Embedding space: `role_person_semantic`

---

### Role

A hat the operator wears: parent, engineer, friend, athlete.

| Property | Notes |
|---|---|
| `name` | Role name |
| `description` | What this role means to the operator |
| `active` | `boolean` — currently being enacted |

Embedding space: `role_person_semantic`

---

### Aspiration

An identity the operator is growing *into* — a becoming, not a target. The pivot of the civic
core: shaped from above by `Role` (who they are now) and from below by `Goal` (what they pursue),
and reshaped as goals are met. No finish line.

| Property | Notes |
|---|---|
| `claim_summary` | The aspiration in the operator's words |
| `description` | What becoming this means |
| `domain` | Life area it concerns |
| `status` | `emerging` \| `developing` \| `integrated` \| `retired` |

Embedding space: `role_person_semantic`

Civic cycle (edges are a follow-up; edge-write path not yet in `life.observe`):
`Role -[:SHAPES]-> Aspiration`, `Aspiration -[:SETS]-> Goal`, `Goal -[:SHAPES]-> Aspiration`.
Agent-ownership of a civic node is expressed via `source_membrane` (e.g. `agent:beacon`), not an edge.

---

### Goal

Something the operator is working toward.

| Property | Notes |
|---|---|
| `title` | Short label |
| `description` | Why this matters |
| `horizon` | `short` \| `medium` \| `long` — time horizon |
| `status` | `active` \| `paused` \| `achieved` \| `retired` |

Embedding space: `goal_system_semantic`

---

### System

A repeatable process or structure that makes a goal sustainable (GTD/James Clear sense).

| Property | Notes |
|---|---|
| `title` | Short label |
| `description` | What this system does |
| `cadence` | How often it runs (free text: `daily`, `weekly`, etc.) |

Embedding space: `goal_system_semantic`

---

### Habit

A recurring behaviour the operator is building, maintaining, or breaking.

| Property | Notes |
|---|---|
| `title` | Short label |
| `trigger` | What initiates the habit |
| `routine` | The behaviour itself |
| `reward` | What follows |
| `status` | `building` \| `stable` \| `breaking` \| `retired` |
| `cadence` | Frequency (free text) |

Embedding space: `goal_system_semantic`

---

### Project

A bounded effort with a start, an end, and a deliverable.

| Property | Notes |
|---|---|
| `title` | Short label |
| `status` | `active` \| `paused` \| `done` \| `abandoned` |
| `deadline` | ISO 8601 date (optional) |

Embedding space: `goal_system_semantic`

---

### Commitment

Something the operator has promised or been assigned — to self, to others, or to a project.

| Property | Notes |
|---|---|
| `title` | Short label |
| `description` | What was committed |
| `due_at` | ISO 8601 timestamp (optional) |
| `status` | `open` \| `fulfilled` \| `broken` \| `deferred` |

Embedding space: `memory_bridge_semantic`

---

### OpenLoop

An unresolved item: undecided, partially done, or waiting.

| Property | Notes |
|---|---|
| `title` | Short label |
| `description` | What's open |
| `loop_type` | `undecided` \| `waiting` \| `partial` \| `forgotten` |
| `status` | `open` \| `resolved` \| `dropped` |

Embedding space: `life_event_semantic`

---

### NextAction

The single concrete next step for a project, goal, or open loop.

| Property | Notes |
|---|---|
| `title` | Short label |
| `context` | Where/when this can be done (free text) |
| `status` | `available` \| `waiting` \| `done` |

Embedding space: `goal_system_semantic`

---

### Routine

A structured sequence of actions that repeats on a schedule.

| Property | Notes |
|---|---|
| `title` | Short label |
| `steps` | `list<string>` |
| `cadence` | Frequency (free text) |

Embedding space: `goal_system_semantic`

---

### Decision

A choice that was made, with the context that led to it.

| Property | Notes |
|---|---|
| `title` | Short label |
| `description` | What was decided |
| `rationale` | Why |
| `decided_at` | ISO 8601 timestamp |

Embedding space: `memory_bridge_semantic`

---

### Preference

A stable personal preference (format, pace, environment, communication style).

| Property | Notes |
|---|---|
| `category` | Domain of the preference |
| `description` | The preference |

Embedding space: `role_person_semantic`

---

### Value

A principle the operator holds.

| Property | Notes |
|---|---|
| `name` | Short name |
| `description` | What this value means in practice |

Embedding space: `role_person_semantic`

---

### Concern

Something the operator is worried about or tracking as a risk.

| Property | Notes |
|---|---|
| `title` | Short label |
| `description` | What and why |
| `severity` | `low` \| `medium` \| `high` |
| `status` | `open` \| `resolved` \| `monitoring` |

Embedding space: `role_person_semantic`

---

### Event

A discrete occurrence: meeting, conversation, health event, milestone.

| Property | Notes |
|---|---|
| `title` | Short label |
| `description` | What happened |
| `occurred_at` | ISO 8601 timestamp |
| `event_type` | Free-text category |

Embedding space: `life_event_semantic`

---

### Signal

A recorded observation persisted into the Life Graph for future reference.

> **Disambiguation**: `Signal` (this node type) is a *graph artifact* — a persisted, provenance-stamped observation. It is different from a *runtime paracrine signal* (seam: `life-graph-paracrine-heartbeat`), which is an ephemeral hotel message. A paracrine signal may create a `Signal` node when the observation is worth keeping, but the two are separate concepts at separate layers.

| Property | Notes |
|---|---|
| `title` | Short label |
| `signal_type` | Category of observation |
| `payload_summary` | What was observed |

Embedding space: `life_event_semantic`

---

### GrowthHypothesis

A belief about what will help the operator improve or change.

| Property | Notes |
|---|---|
| `title` | Short label |
| `hypothesis` | The claim |
| `domain` | Area of life this concerns |
| `status` | `proposed` \| `testing` \| `confirmed` \| `rejected` |

Embedding space: `skill_tool_semantic`

#### Legacy repo-implementation idea convention (`idea:<slug>`)

Operator ideas captured by the idea-intake charter
([ARIA_IDEA_PIPELINE_PROPOSAL](../ARIA_IDEA_PIPELINE_PROPOSAL.md)) reuse
`GrowthHypothesis` when they are specifically proposals for changing Philotic
itself. This remains a project-triage compatibility convention; it is not the
owner of the personal creative-learning lifecycle.

| Convention | Value |
|---|---|
| `id` | `idea:<slug>` (e.g. `idea:healthkit-observe`) |
| `claim_summary` | The idea in 1–2 sentences, operator's words preserved in the evidence packet |
| `idea_kind` | `implementation` — set at triage time (see `idea_status` note) |
| `target` | `philotic-stack` (later: `muninndb`, `home`, …) — set at triage time |
| `idea_status` | absent = `captured` → `promoted` (with `graph_ref: doc:<proposal-id>`) → `shipped` \| `declined` (with `idea_status_reason`). Written by the triage pipeline (`just idea-sweep`), not at intake — `life.observe` has no custom-property write path |

Provenance envelope as always (`source_membrane`, `observed_by`,
`validation_state: proposed`). The LifeGraph node is the provenance anchor;
the Intel Graph node referenced by `graph_ref` owns repository execution
state. Personal questions and creative ideas use the V006 labels below.

---

### Question / Idea / Experiment / Artifact / Learning / Source

The minimal creative-learning vocabulary added by V006. These labels describe
movement through a creative thread, not a new store:

| Label | Meaning | Initial `creative_status` |
|---|---|---|
| `Question` | Something worth understanding | `captured` |
| `Idea` | A possible connection, approach, or creation | `captured` |
| `Experiment` | A bounded way to test or explore | `captured` |
| `Artifact` | Something made, published, performed, or shipped | `captured` |
| `Learning` | A reusable conclusion grounded in evidence | `captured` |
| `Source` | Material that informed the work | `captured` |

All six use `creative_learning_semantic`. Quick capture creates proposed nodes;
`life.commit` remains the only confirmation path. Advancement is represented
with typed relationships rather than by copying the same content between
stores.

---

### GrowthExperiment

A time-bounded test of a `GrowthHypothesis`.

| Property | Notes |
|---|---|
| `title` | Short label |
| `description` | What is being tried |
| `started_at` | ISO 8601 timestamp |
| `ends_at` | ISO 8601 timestamp |
| `outcome` | Free text — filled when complete |
| `status` | `planned` \| `active` \| `completed` \| `abandoned` |

Embedding space: `skill_tool_semantic`

---

### DriftFinding

A recorded observation that the system is drifting in an unhelpful direction.

| Property | Notes |
|---|---|
| `title` | Short label |
| `description` | What drift was detected |
| `drift_type` | e.g. `nagging`, `stale_facts`, `productivity_bias`, `graph_clutter` |
| `status` | `open` \| `addressed` \| `wont_fix` |

Embedding space: `skill_tool_semantic`

---

### CapabilityPatch / SkillPatch / ToolPatch / SchemaPatch / AttentionPatch / SystemPatch

Proposed or applied improvements to the Life Graph OS itself.

All patch types share:

| Property | Notes |
|---|---|
| `title` | Short label |
| `description` | What the patch does |
| `risk_tier` | `safe_auto` \| `confirm_first` \| `proposal_only` |
| `status` | `proposed` \| `applied` \| `deferred` \| `reverted` \| `rejected` |
| `applied_at` | ISO 8601 timestamp (optional) |

Embedding space: `skill_tool_semantic`

---

## Relationship Types

| Relationship | Typical source | Typical target | Notes |
|---|---|---|---|
| `OWNS` | `Role`, `Person` | `Goal`, `Project`, `System`, `Habit` | Primary ownership |
| `SUPPORTS` | `System`, `Habit`, `Routine` | `Goal`, `Habit` | Contributes to |
| `CONTAINS` | `Project`, `System`, `Routine` | `NextAction`, `Habit`, `OpenLoop` | Structural membership |
| `ADVANCES` | `NextAction`, `Habit`, `Project` | `Goal` | Makes progress toward |
| `BLOCKED_BY` | `Goal`, `NextAction`, `Project` | `Concern`, `OpenLoop`, `Commitment` | Currently blocked |
| `PROMISED_TO` | `Commitment` | `Person` | Committed to this person |
| `RECURS` | `Habit`, `Routine` | `System` | Recurring element of a system |
| `NEEDS_FOLLOWUP` | `Event`, `Commitment`, `OpenLoop` | `NextAction`, `Commitment` | Requires action |
| `SUPERSEDES` | `Decision`, `SchemaPatch` | `Decision`, `SchemaPatch` | Replaces an earlier record |
| `CONTRADICTS` | `Signal`, `Commitment`, `Decision` | `Signal`, `Commitment`, `Decision` | Conflicting facts; triggers adjudication |
| `EVIDENCED_BY` | any node | `Signal`, `Event` | Grounded by this observation |
| `REDUCES_FRICTION_FOR` | `System`, `Habit`, `Routine` | `Role`, `Goal`, `Habit` | Explicitly reduces barrier |
| `SUGGESTS_PATCH` | `DriftFinding`, `GrowthExperiment` | `*Patch` | Leads to a patch proposal |
| `APPLIES_TO_ROLE` | `Preference`, `Value`, `Concern`, `*Patch` | `Role` | Scoped to a specific role |
| `INSPIRES` | `Question` | `Idea` | Curiosity generated a possible direction |
| `INFORMS` | `Source` | `Question`, `Idea` | Source materially shaped the thread |
| `TESTED_BY` | `Idea` | `Experiment` | Idea has a bounded test |
| `PRODUCES` | `Experiment` | `Artifact`, `Learning` | Test yielded something made or learned |
| `EXPRESSES` | `Artifact` | `Idea` | Artifact embodies the idea |
| `REFINES` | `Learning` | `Idea`, `Goal`, `System` | Learning changed a future direction or method |
| `SHARED_WITH` | `Artifact` | `Person`, `Role` | Artifact reached an audience or collaborator |
| `SCOPED_TO` | any observed node | `Role` | Server-injected structural/domain anchor |

Edge provenance: the full provenance envelope applies to agent-inferred edges. Operator-asserted edges may carry only `source_membrane` and `observed_at`.

---

## Vector Spaces

All spaces use dimension `768` and metric `cos` (cosine similarity). Never mix models or dimensions within a space. V003 migrated the original 1536d indexes to the deployed local ONNX baseline, `Xenova/all-mpnet-base-v2`.

| Space | Node labels | Purpose |
|---|---|---|
| `life_event_semantic` | `Event`, `Signal`, `OpenLoop` | Temporal and observational recall |
| `goal_system_semantic` | `Goal`, `System`, `Habit`, `Project`, `Routine`, `NextAction` | Purpose, structure, and next-step retrieval |
| `skill_tool_semantic` | `GrowthHypothesis`, `GrowthExperiment`, `DriftFinding`, `CapabilityPatch`, `SkillPatch`, `ToolPatch`, `SchemaPatch`, `AttentionPatch`, `SystemPatch` | Capability improvement retrieval |
| `creative_learning_semantic` | `Question`, `Idea`, `Experiment`, `Artifact`, `Learning`, `Source` | Creative-thread re-entry, making, reflection, and reuse |
| `role_person_semantic` | `Role`, `Person`, `Value`, `Preference`, `Concern` | Identity and relational context |
| `memory_bridge_semantic` | `Commitment`, `Decision` | Cross-domain bridges: commitments and decisions span roles, goals, and people |

Retrieval pipeline:
1. semantic vector search finds candidate nodes in the relevant space
2. graph expansion follows bounded typed paths from those candidates
3. policy filters remove stale (`validation_state = retired`), overconfident, or context-inappropriate candidates
4. role-aware ranking scores by active role, recent session state, and explicit operator intent
5. context projection emits a bounded `EvidencePacket` with source refs — not raw graph sprawl

---

## Migration Notes

- Migration target: Memgraph 3.10.1 on `vps-jane` (Tailscale `100.64.212.8:7687`)
- SQLite provider (`SqliteCypherProvider`) does **not** support DDL. No SQLite migration exists or is needed — Life Graph OS is Memgraph-only.
- Apply `V001__life_graph_schema.cypher` statement-by-statement (Bolt does not support multi-statement batches).
- `V006__creative_learning_flywheel.cypher` was applied to the live
  `vps-jane` Memgraph on 2026-07-24 after parser validation. It adds the
  creative constraints, property indexes, and 768-dimensional semantic
  indexes.
- Migration is designed to be applied once. Re-running will fail on existing constraints; that is expected.
- Verification on 2026-07-24 used `SHOW CONSTRAINT INFO;`, `SHOW INDEX INFO;`,
  and `SHOW VECTOR INDEX INFO;` to confirm the six label constraints, property
  indexes, and six cosine vector indexes at 768 dimensions.
