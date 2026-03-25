---
title: "Context Graph Tool Runner"
doc_type: proposal
domain: tooling-execution
status: proposed
last_updated: 2026-03-17
tags:
  - graph
  - tool-runner
  - knowledge-graph
  - shared-state
  - multi-identity
  - replication
related_docs:
  - TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md
  - TOOL_MANAGEMENT_PLANE_PROPOSAL.md
  - ARCHITECTURE_STATUS.md
task_refs:
  - docs/task.md
proposal_id: context-graph-runner
active_seams:
  - graph-runner-store
  - graph-runner-visibility
  - graph-runner-tool-surface
  - graph-runner-hotel-registry
---

# Context Graph Tool Runner

## Goal

Provide a first-class shared graph store accessible to any agent or identity in the
hotel, exposed as standard tools via the normal IPC routing path.

The graph is not a control-plane artifact. It is a **shared deliverable** — a
structured, identity-aware, multi-tenant project graph that accumulates research
findings, roles, goals, decisions, and their relationships as agents do real work
together.

This is distinct from:

- the aiua context graph (session, guest, and apartment state — control plane)
- Muninn (semantic recall / engram store — memory plane)
- workspace tools (filesystem access — execution plane)

---

## Core Recommendation

### New crate: `graph-runner`

A standalone guest binary that:

1. Physically owns **1-N named graphs** in an embedded SQLite database
2. Enforces per-graph **user-defined schemas** (node types and edge types)
3. Evaluates **per-node visibility rules** on every read so callers cannot bypass access control
4. Registers with the hotel as a Philotic guest on role `tool.graph`
5. Exposes all graph operations as standard `tool.graph` capability tools
6. Writes a **hotel registry entry** (`graph_id → instance`) on `graph.create` so philote can route graph-specific calls to the correct instance

Storage is abstracted behind a `GraphStore` trait from day one, enabling the
replication path (Option C) without changing the tool surface.

---

## Disposition

`in-progress` — Slices 1–3 shipped.

- [x] Slice 1: CRUD, visibility, FTS, traversal, 9 access tests + 20 store tests
- [x] Slice 2: Hotel registry write (`RegisterGraphInstance` IpcRequest), traversal direction/depth/filter tests, FTS visibility tests (29 total tests)
- [x] Slice 3: Table adapter — `TableStore` trait, `GraphTableStore` supertrait, `table.*` tool namespace (10 tools), `table_ref` on Node, 38 total tests
- [ ] Slice 4: Export (`graph.export` — JSON / Mermaid / DOT)
- [ ] Slice 5: Replication foundation (Option C)

---

## Multi-Instance Model

Multiple `graph-runner` instances may run within a hotel. Each instance owns an
exclusive set of graphs — no shared state between instances.

### Instance roles

```
role: "primary"    ← owns writes for a graph
role: "replica"    ← read-only copy, fed by change events from primary (future)
```

The hotel registry uses `role` from day one so adding replication later is a
registry update + event subscription, not a schema change.

### Routing

Tool calls for a specific graph carry a `graph_id` argument. Philote resolves the
owning instance from the hotel registry before dispatch, using `target_guest_id`
in the IPC envelope.

`graph.create` is the only call that does not require a pre-existing registry
entry — it goes to any available `tool.graph` instance. The instance writes the
registry entry atomically as part of graph creation.

### Replication path (future — Option C)

When replication is added:

1. The primary instance emits `graph.changed` mesh events on each write
2. Replica instances subscribe and apply changes to their own SQLite
3. The hotel registry gains a `replica_of` link pointing at the primary
4. Read routing can resolve to any registered replica; writes always go to primary
5. `GraphStore` trait gains a `ReplicatedGraphStore` implementation — tool surface unchanged

---

## Graph Instance Model

Each graph is created with a user-defined **schema** — a vocabulary of node types
and edge types. Schema is additive (new types may be added; used types cannot be
removed). The runner enforces schema on write; lenient mode allows schemaless
bootstrapping.

### Graph metadata

```
graph_id          ← stable ULID
name              ← unique within the runner instance
description
schema            ← { node_types, edge_types, strict: bool }
default_visibility ← "private" | "hotel-public" | "public"
creator
created_at
```

### Node

```
node_id           ← stable ULID
graph_id
node_type         ← from graph schema (e.g. "Research", "Goal", "Role", "Decision")
label             ← human-readable name
content           ← JSON blob, freeform structured data per node type
tags              ← Vec<String>, for filtering
visibility        ← Vec<String>, access control tags (see below)
creator           ← identity string
created_at, updated_at, deleted_at (soft delete)
```

### Edge

```
edge_id           ← stable ULID
graph_id
from_node_id
to_node_id
edge_type         ← from graph schema (e.g. "SUPPORTS", "BLOCKS", "ASSIGNED_TO")
label             ← optional
content           ← JSON blob
visibility        ← Vec<String>, inherits most-restrictive of both endpoints + own tags
creator           ← identity string
created_at, updated_at, deleted_at (soft delete)
```

---

## Visibility Rules

Visibility is evaluated on every read inside the `GraphStore` implementation.
Callers cannot bypass it.

### Tag vocabulary

| Tag | Meaning |
|---|---|
| `"public"` | Visible to any identity |
| `"hotel-public"` | Visible to any identity within the hotel |
| `"role:<name>"` | Visible if identity has the named role |
| `"identity:<id>"` | Visible if identity matches exactly |

### Resolution order

1. If the node's `visibility` list is empty: apply the graph's `default_visibility`
2. Otherwise: a node is visible if **any** tag in the list matches the requesting identity
3. An edge is visible only if **both** endpoint nodes are visible AND the edge's own visibility permits

The `creator` field is metadata, not a visibility tag. If a creator wants
self-only visibility they use `["identity:<their_id>"]`.

---

## Storage Abstraction

```rust
pub trait GraphStore: Send + Sync {
    fn create_graph(&self, spec: GraphSpec) -> Result<String>;
    fn get_graph(&self, graph_id: &str) -> Result<Option<GraphMeta>>;
    fn list_graphs(&self) -> Result<Vec<GraphMeta>>;
    fn update_schema(&self, graph_id: &str, schema: GraphSchema) -> Result<()>;

    fn upsert_node(&self, graph_id: &str, input: NodeInput) -> Result<String>;
    fn get_node(&self, graph_id: &str, node_id: &str, identity: &Identity) -> Result<Option<Node>>;
    fn list_nodes(&self, graph_id: &str, filter: &NodeFilter, identity: &Identity) -> Result<Vec<Node>>;
    fn delete_node(&self, graph_id: &str, node_id: &str) -> Result<()>;

    fn upsert_edge(&self, graph_id: &str, input: EdgeInput) -> Result<String>;
    fn get_edge(&self, graph_id: &str, edge_id: &str, identity: &Identity) -> Result<Option<Edge>>;
    fn list_edges(&self, graph_id: &str, filter: &EdgeFilter, identity: &Identity) -> Result<Vec<Edge>>;
    fn delete_edge(&self, graph_id: &str, edge_id: &str) -> Result<()>;

    fn traverse(&self, graph_id: &str, query: &TraversalQuery, identity: &Identity) -> Result<TraversalResult>;
    fn search_nodes(&self, graph_id: &str, query: &str, identity: &Identity) -> Result<Vec<Node>>;
}
```

`ReadCtx` carries identity for visibility enforcement. All implementations must
enforce visibility — it is not the caller's responsibility.

### Slice 1 implementation: SQLite + rusqlite + FTS5

`rusqlite 0.37` with the `bundled` feature is already in the workspace
(ansible-mesh-core). Application-layer BFS/DFS for traversal (~150 lines).
FTS5 virtual table for full-text search over label and content.

Single SQLite file per runner instance. Path: `PHILOTIC_GRAPH_DB_PATH` env var,
defaulting to `~/.philotic/graph-runner.db`.

### Future: SurrealDB (embedded → server)

Named migration target. Native graph traversal eliminates the BFS/DFS application
layer. Embedded mode → server mode enables multi-hotel shared graphs. The trait
boundary makes the swap a single-file change.

---

## Tool Surface

The guest registers on role `tool.graph` and advertises:

```
graph.create             ← create a named graph with schema; writes hotel registry entry
graph.list               ← list graphs on this instance
graph.schema.get         ← get type vocabulary for a graph
graph.schema.update      ← add node/edge types (additive only)

graph.node.upsert        ← create or update a node
graph.node.get           ← get by ID
graph.node.list          ← list with type / tag / creator filters
graph.node.delete        ← soft-delete

graph.edge.upsert        ← create or update a directed edge
graph.edge.get           ← get by ID
graph.edge.list          ← list edges from/to a node, with type filter
graph.edge.delete        ← soft-delete

graph.traverse           ← BFS/DFS from a node, depth-limited, visibility-aware
graph.search             ← full-text search across nodes in a graph

graph.export             ← export subgraph as JSON (DOT/Mermaid later)
```

---

## Hotel Registry Integration

On `graph.create` the runner writes a registry entry to the hotel context graph:

```json
{
  "entity_type": "graph_runner_registry",
  "graph_id": "<ulid>",
  "graph_name": "<name>",
  "instance_id": "<guest_id>",
  "role": "primary",
  "registered_at": "<timestamp>"
}
```

Philote uses this to resolve `target_guest_id` when dispatching graph-specific
tool calls. The seam for this write is `graph-runner-hotel-registry`.

---

## Slice Sequence

### Slice 1 — Foundation + core CRUD (current)

- `crates/graph-runner` crate scaffold
- `GraphStore` trait + SQLite implementation
- Per-graph schema: node types, edge types, strict mode flag
- Visibility evaluation on all reads
- IPC guest loop on role `tool.graph`
- Hotel registry write on `graph.create`
- Tools: `graph.create`, `graph.list`, `graph.schema.get`,
  `graph.node.upsert`, `graph.node.get`, `graph.node.list`,
  `graph.edge.upsert`, `graph.edge.get`, `graph.edge.list`
- Tests: schema enforcement, visibility rules, CRUD round-trips

### Slice 2 — Traversal + search

- BFS/DFS traversal with depth cap and visibility-aware edge filtering
- FTS5 full-text search across node labels + content
- Tools: `graph.traverse`, `graph.search`
- Tests: traversal correctness, visibility gate on traversal

### Slice 3 — Schema evolution + soft-delete lifecycle

- Schema additive update with used-type validation
- Soft-delete with audit trail for nodes and edges
- Tools: `graph.schema.update`, `graph.node.delete`, `graph.edge.delete`

### Slice 4 — Export

- Subgraph export as JSON
- Mermaid / DOT format for visualization
- Tools: `graph.export`

### Slice 5 — Replication foundation (Option C)

- `graph.changed` mesh event emission on every write (primary instances)
- Replica instance subscription and apply loop
- Hotel registry `replica_of` linkage
- Read routing to replicas; write routing enforced to primary

---

## Current Slice

**Slice 1** — in progress.

---

## Open Questions

- **Hotel registry write transport**: `graph.create` must write to the hotel context
  graph. Current path: the runner sends an `IpcRequest::EmitTask` targeting aiua's
  registry role, same pattern as other guests writing durable state. Confirm the
  correct IPC verb before Slice 1 lands.
- **`graph_id` in tool envelopes**: philote must include `graph_id` in the task
  JSON for all graph-specific tool calls so the hotel can route to the correct
  instance. Confirm the routing mechanism (hotel-side lookup vs. philote-side
  pre-resolution) before Slice 2.
- **Replica consistency model**: eventual (mesh events) vs. synchronous (2PC).
  Defer decision to Slice 5; document the seam now.
