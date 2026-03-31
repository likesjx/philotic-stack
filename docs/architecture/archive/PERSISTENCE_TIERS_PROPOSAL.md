---
title: Philotic Persistence Tiers
doc_type: proposal
domain: memory-context
status: proposed
last_updated: 2026-03-31
tags:
- ods
- graph
- muninn
- data-adapter
- agent-graph
- persistence
- architecture
related_docs:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
- CONTEXT_GRAPH_RUNNER_PROPOSAL.md
- TOOL_MANAGEMENT_PLANE_PROPOSAL.md
- MUNINN_MEMORY_PROTOCOL_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: persistence-tiers
active_seams:
- ods-rename
- agent-graph-materialization
- graph-adapter-table-surface
- mesh-graph-distribution
---

# Philotic Persistence Tiers

## Goal

Define the full persistence architecture for the Philotic Stack — what stores
exist, what each one owns, and how they relate. This document replaces the
implicit assumption that aiua's SQLite is "the context graph" and that all
durable state belongs there.

---

## The Problem With One Store

The current codebase has a single durable SQLite owned by aiua, historically
called the **context graph**. It contains:

- hotel operational state (guests, leases, mesh events, boot config)
- session and turn history (content)
- tool and skill catalog (definitions)
- role definitions (agent identity)

These have fundamentally different access patterns, lifetimes, query models,
and portability requirements. Keeping them together was expedient. It is now a
misnomer and a scaling ceiling.

---

## The Three Tiers

```
┌─────────────────────────────────────────────────────────────────┐
│  ODS  (Operational Data Store)                                  │
│  Hotel-local. Operational state only. Fast. Boot-critical.      │
├─────────────────────────────────────────────────────────────────┤
│  Graph Data Adapters                                            │
│  ┌──────────────────────────┐  ┌──────────────────────────────┐ │
│  │  Agent Graph             │  │  User / Project Graphs       │ │
│  │  system:agents           │  │  explicit, named, owned      │ │
│  │  mesh-distributed        │  │  by the human                │ │
│  │  shared garden           │  │  all about them              │ │
│  └──────────────────────────┘  └──────────────────────────────┘ │
│  ┌──────────────────────────┐                                   │
│  │  Table Adapter           │                                   │
│  │  structured tabular data │                                   │
│  │  linked via node pointer │                                   │
│  └──────────────────────────┘                                   │
├─────────────────────────────────────────────────────────────────┤
│  Muninn  (Atomic Memory)                                        │
│  Engrams. Semantic recall. Mesh-distributed. Agent-portable.    │
└─────────────────────────────────────────────────────────────────┘
```

---

## Tier 1: ODS — Operational Data Store

### What it is

The hotel's operational machinery. Just enough persistent state to keep the
hotel running. Renamed from "context graph" — it is not a graph, never was.

### What it owns

- Guest materialization state (PIDs, process supervision records)
- Runtime authority leases (poll leases, execution leases)
- Mesh events and cursors (inter-hotel coordination, append-only)
- Boot manifest (which guests to spawn and with what config)
- Tool and skill access index (who has access to what — operational routing)
- Binary resolution records

### What it does NOT own

- Session or turn content
- Tool or skill definitions (those are content, not routing index)
- Role or agent profile definitions
- Any content an agent produces or consumes

### Characteristics

- Hotel-local. Not distributed. Not portable.
- SQLite. Single writer (aiua). Fast indexed reads.
- Boot-critical — if this is unavailable, the hotel cannot start.
- Should be as small as possible. Operational state only.

### Rename note

All references to "context graph" in the aiua/control-plane sense should be
updated to "ODS" or "hotel ODS". The `GraphStorage` trait in
`ansible-mesh-core` should be renamed `OdsStorage`.

---

## Tier 2: Graph Data Adapters

Graph data adapters are **not tool runners**. A tool runner is stateless and
compute-oriented. A graph data adapter owns persistent shared state, mediates
access, enforces visibility rules, and has instance ownership semantics.

All graph data adapters share the same `graph-runner` binary. The runner owns
1-N graph instances in a single SQLite file per runner instance. Multiple
runner instances may exist within a hotel; the hotel registry (in the ODS)
maps `graph_id → instance_id`.

See [CONTEXT_GRAPH_RUNNER_PROPOSAL.md](CONTEXT_GRAPH_RUNNER_PROPOSAL.md) for
implementation detail.

### The Agent Graph — The Shared Garden

```
graph_id: system:agents
```

One graph. Mesh-distributed. Always present on every hotel in the mesh.
The shared garden where agents grow, collaborate, and leave durable records
of who they are and what they know.

**What lives here:**

- `AgentProfile` nodes — persona, capabilities, working style
- `RoleDefinition` nodes — what a role does, its responsibilities
- `Decision` nodes — recorded decisions with full rationale
- `Goal` nodes — objectives, active commitments
- `SkillDefinition` nodes — the content side of the skill catalog
- `ToolDefinition` nodes — rich tool descriptions, schemas, relationships
- Session records and turn history (content layer, not the operational handle)

**The multi-tenant model:**

Every node an agent writes defaults to `visibility: ["identity:{agent_id}"]`.
That node belongs to the agent's slice — invisible to other agents by default.

Cross-agent sharing is explicit and intentional:
- `visibility: ["role:researcher"]` — visible to all agents with that role
- `visibility: ["identity:agent-a", "identity:agent-b"]` — shared between
  two specific agents
- `visibility: ["public"]` — visible to all

This makes the shared garden a real collaborative space: agents work in their
own plot by default, but can plant things others can see and build on.

**Distribution:**

Every write to the agent graph emits a mesh event. All hotels with a copy
apply the update. An agent materializing on a new hotel subscribes to
`system:agents` and catches up. The agent's slice is available wherever
the agent goes.

**Materialization:**

The agent graph is provisioned as part of standard philote materialization.
The agent does not create it — it already exists. Materializing an agent
means: ensure `system:agents` is present and synced, then begin writing
the agent's slice into it.

### User / Project Graphs — All About Them

These are the human's graphs. Explicit, named, created on demand.

A user creates a graph to track a body of work:

```
graph.create({
  name: "q2-research",
  schema: {
    node_types: ["Finding", "Question", "Source", "Goal", "Decision"],
    edge_types: ["ANSWERS", "SUPPORTS", "BLOCKS", "DERIVED_FROM"]
  },
  default_visibility: "private"
})
```

**What lives here:**

- Research findings, questions, sources
- Goals, milestones, success criteria
- Decisions with context and alternatives
- Roles and responsibilities (from the human perspective)
- Anything the user wants to accumulate into a coherent structure

These graphs are the user's persistent knowledge work. They outlive sessions.
They can be shared (selectively, via visibility tags) with agents or other
users. They can be replicated across hotels if the user wants redundancy.

They are not operational. They are not the agent's garden. They are
the human's record of what matters to them.

### The Table Adapter

Structured tabular data — uniform rows, typed columns, filter/sort/aggregate
queries — lives alongside the graph in the same runner binary under the
`table.*` tool namespace.

A graph node can optionally carry a `table_ref` field pointing to a table ID.
The node is the graph-level pointer and metadata anchor (what this table
represents, how it relates to other things). Reading actual rows always
requires an explicit `table.query` call — the graph never returns row data.

```
graph node (type: "Dataset")  →  table_ref: "tbl_01abc..."
                                        ↓
                                 table.query { table_id: ... }
                                        ↓
                                   actual rows
```

Graph visibility and table access are independent permission decisions.
An identity can see the node exists without having access to query its rows.

---

## Tier 3: Muninn — Atomic Memory

Muninn is not a graph adapter. It is the **atomized thought layer**.

**What it stores:**

- Engrams: one fact, one observation, one decision per entry
- Short content by design (1-3 sentences)
- Links between engrams (Supports, Contradicts, DerivedFrom, etc.)

**Query model:** semantic recall via embeddings — "what do I know that is
relevant to this moment?" Not traversal. Not filter. Similarity.

**Relationship to the graph adapters:**

Muninn and the agent graph are complementary, not overlapping:

| | Muninn | Agent Graph |
|---|---|---|
| Content shape | Atomic, short | Prose, rich |
| Update frequency | High churn | Mostly static |
| Query model | Semantic similarity | Traversal, filter |
| Purpose | Working memory | Durable identity + knowledge record |

The same event may produce both. A decision might generate:
- A Muninn engram: `"decided to use SQLite for graph-runner storage"` (for
  quick semantic recall in future sessions)
- An agent graph `Decision` node with full rationale, alternatives considered,
  outcome, and edges to related decisions (for structured traversal and audit)

---

## Query Model Summary

| Tier | Primary question | Query mechanism |
|---|---|---|
| ODS | Is the hotel running correctly? | Indexed SQL |
| Agent graph | What is the record of who we are and what we've done? | Traversal, filter, FTS |
| Project graphs | What do I know about this body of work? | Traversal, filter, FTS |
| Table adapter | What rows match these criteria? | Filter, sort, aggregate |
| Muninn | What's relevant right now? | Semantic similarity |

---

## Distribution Model

| Tier | Distribution | Portability |
|---|---|---|
| ODS | Hotel-local | Not portable — operational to this hotel |
| Agent graph (`system:agents`) | Mesh-distributed | Fully portable — follows every agent |
| Project graphs | Hotel-local by default; opt-in replication | Portable if replicated |
| Table adapter | Same as owning runner instance | Same as project graphs |
| Muninn | Mesh-distributed | Fully portable — follows every agent |

---

## What This Means For The ODS Rename

The current `GraphStorage` trait in `ansible-mesh-core` and all associated
code should be renamed to reflect that it serves the ODS, not a general graph:

- `GraphStorage` → `OdsStorage`
- `SqliteGraphStorage` → `SqliteOdsStorage`
- "context graph" in control-plane prose → "ODS" or "hotel ODS"
- "context graph" in agent-facing prose → "agent graph" or the specific named graph

This rename is mechanical in code and conceptual in docs. It should be done
in a dedicated `codex/ods-rename` workstream.

---

## Inter-Agent Communication

The agent graph is shared state, not a conversation channel. Agents also need
to talk to one another directly. Three shapes serve different needs:

### Shape 1 — Async graph message

Agent A writes a `Message` node in `system:agents` with
`visibility: ["identity:agent-b"]`. Persistent, auditable, not real-time.
Use when handing off context, leaving a note, or coordinating across a
session boundary.

### Shape 2 — Direct IPC

Agent A emits a task directly to Agent B's `guest_id` or role via
`EmitTask`. No session overhead. Lightweight coordination, delegation,
quick signals. Already supported by the IPC routing layer — needs a defined
inbox convention on the receiving agent.

Crosses hotel boundaries naturally via mesh routing.

### Shape 3 — Inter-agent session

Agent A and Agent B are both participants in a session. Same turn loop,
same tool and approval infrastructure as human→agent conversations. Use
when the work requires real back-and-forth, shared tool use, and a joint
cognitive loop.

### The judgment layer

The runtime provides all three shapes as capabilities. The **agent profile**
in `system:agents` encodes default preferences per interaction type. **Skills**
encode the how and when for specific interaction patterns.

```
runtime provides:    async graph  +  direct IPC  +  inter-agent session
agent profile says:  default preferences per interaction type
skills say:          protocol for specific patterns
agent graph records: lightweight audit nodes for all interactions
```

A `INTER_AGENT_COMMUNICATION_PROPOSAL.md` should formalize the inbox
convention and session participation model when the time comes.

---

## Disposition

`proposed` — all decisions confirmed in design session 2026-03-17.
Implementation in progress:

- [x] `graph-runner` Slice 1 shipped (CRUD, visibility, traversal, FTS, 22 tests)
- [x] `graph-runner` Slice 2 shipped (hotel registry, extended traversal + FTS tests, 29 tests)
- [x] `graph-runner` Slice 3 shipped (table adapter, 38 tests)
- [ ] ODS rename (`GraphStorage` → `OdsStorage`)
- [ ] Agent graph provisioning at philote materialization
- [ ] Mesh distribution for agent graph (graph-runner Slice 5 / Option C)

---

## Current Slice

`graph-runner` Slice 3 shipped:
- [x] `TableStore` trait: `create_table`, `get_table`, `list_tables`, `update_table`, `drop_table`, `insert_row`, `get_row`, `update_row`, `delete_row`, `query_rows`
- [x] `GraphTableStore` supertrait combining `GraphStore + TableStore` — single `&dyn GraphTableStore` dispatch object
- [x] `table_ref: Option<String>` first-class field on `NodeInput` / `Node` with SQLite column + index
- [x] SQLite schema: `tables` + `table_rows` with soft-delete on rows; additive-only column enforcement on `update_table`
- [x] `table.*` tool namespace (10 tools): wired into IPC dispatch in main.rs
- [x] 9 table adapter tests + round-trip test for `table_ref` on nodes
- [x] 38 total tests passing

Slice 4 candidates:
- Subgraph export: `graph.export` tool returning JSON / Mermaid / DOT
- Agent graph provisioning at philote materialization
