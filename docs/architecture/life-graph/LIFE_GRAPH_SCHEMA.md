---
title: Life Graph Schema
doc_type: specification
domain: memory-context
status: proposed
last_updated: 2026-06-04
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
- **Migration**: `migrations/V001__life_graph_schema.cypher`

The `graph-datasource` crate remains generic. It does not know that a graph contains life data. Life Graph OS semantics live in `data-memorygraphrag` and in this schema vocabulary.

## Provenance Envelope

Every node and every agent-written or inferred edge **must** carry these properties:

| Property | Type | Description |
|---|---|---|
| `source_membrane` | `string` | Membrane or agent that wrote this record (e.g. `membrane:telegram`, `agent:beacon`) |
| `provenance` | `string` | Claim origin: `user_input`, `transcript`, `calendar`, `health_data`, `agent_inferred`, `operator_confirmed` |
| `confidence` | `float` | 0.0–1.0. Inferred facts start low; rise with evidence and operator confirmation |
| `validation_state` | `string` | `inferred` \| `proposed` \| `confirmed` \| `retired` \| `conflicted` |
| `observed_at` | `string` | ISO 8601 timestamp when first observed |
| `last_confirmed_at` | `string \| null` | ISO 8601 timestamp of last operator or strong-evidence confirmation |

Operator-created nodes may omit provenance fields; they are required on agent-written records.

## Embedding Metadata

Nodes that participate in semantic retrieval carry these additional properties alongside the `embedding` vector:

| Property | Type | Description |
|---|---|---|
| `embedding` | `list<float>` | The vector. Indexed by the node's vector index. |
| `embedding_model` | `string` | Model ID used to produce the vector (e.g. `text-embedding-3-small`) |
| `embedding_model_gen` | `int` | Generation counter — increment when the model changes to trigger re-embedding |
| `embedding_dims` | `int` | Dimension. Must match the vector index config (`1536` baseline) |
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

Edge provenance: the full provenance envelope applies to agent-inferred edges. Operator-asserted edges may carry only `source_membrane` and `observed_at`.

---

## Vector Spaces

All spaces use dimension `1536` and metric `cos` (cosine similarity). Never mix models or dimensions within a space.

| Space | Node labels | Purpose |
|---|---|---|
| `life_event_semantic` | `Event`, `Signal`, `OpenLoop` | Temporal and observational recall |
| `goal_system_semantic` | `Goal`, `System`, `Habit`, `Project`, `Routine`, `NextAction` | Purpose, structure, and next-step retrieval |
| `skill_tool_semantic` | `GrowthHypothesis`, `GrowthExperiment`, `DriftFinding`, `CapabilityPatch`, `SkillPatch`, `ToolPatch`, `SchemaPatch`, `AttentionPatch`, `SystemPatch` | Capability improvement retrieval |
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
- Migration is designed to be applied once. Re-running will fail on existing constraints; that is expected.
- Verification: after applying, run `SHOW INDEX INFO;` and `SHOW VECTOR INDEX INFO;` to confirm all constraints, indexes, and vector indexes are present.
