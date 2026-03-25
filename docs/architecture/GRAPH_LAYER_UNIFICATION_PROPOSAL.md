---
title: "Graph Layer Unification"
doc_type: proposal
domain: runtime-sessions
status: proposed
last_updated: 2026-03-22
tags:
  - graph
  - storage
  - refactor
  - graph-adapter
  - graph-domain
  - context-graph
  - active-seam
related_docs:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
  - AGENT_RESOURCE_MODEL_PROPOSAL.md
  - CONTEXT_GRAPH_RUNNER_PROPOSAL.md
  - TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: graph-layer-unification
active_seams:
  - graph-domain-layer
  - graph-adapter-migration
  - graph-store-instances
---

# Graph Layer Unification

## Goal

Eliminate the two-headed graph abstraction in `ansible-mesh-core`. Today `GraphAdapter`
(generic node/edge primitives) and `GraphStorage` (domain operations going straight to SQL)
are completely disconnected. Every new entity type requires a new SQL table, a new
`GraphStorage` method, and a new `SqliteGraphStorage` impl — three places for one concept.

The fix is a single reusable middle layer, `GraphDomain`, that owns all domain logic and
is expressed entirely in terms of `GraphAdapter`. Domain operations live once; backends
become interchangeable.

---

## Core Recommendation

### Three-layer stack

```
┌──────────────────────────────────────────────────────┐
│  Layer 3 — Graph store instances                      │
│  Hotel CG · Agent graph · Training trace store        │
│  Each is a GraphDomain over its own GraphAdapter      │
└──────────────┬───────────────────────────────────────┘
               │  Arc<GraphDomain>
┌──────────────▼───────────────────────────────────────┐
│  Layer 2 — GraphDomain  (new)                         │
│  All domain ops expressed as GraphAdapter primitives  │
│  Holds Arc<dyn GraphAdapter>                          │
│  Lives in ansible-mesh-core — no SQL, no backend      │
└──────────────┬───────────────────────────────────────┘
               │  Arc<dyn GraphAdapter>
┌──────────────▼───────────────────────────────────────┐
│  Layer 1 — GraphAdapter  (already exists)             │
│  upsert_node / get_node / delete_node / list_nodes_by_kind  │
│  upsert_edge / delete_edge / list_edges_from          │
│  One impl per backend: SqliteGraphAdapter, RocksDbGraphAdapter  │
│  Zero domain knowledge                                │
└──────────────────────────────────────────────────────┘
```

### Layer 1 — GraphAdapter

Already defined in `storage.rs` (line 450). No changes needed to the trait. The current
`SqliteGraphStorage` SQL for nodes and edges is extracted into a new `SqliteGraphAdapter`
implementing only this trait.

### Layer 2 — GraphDomain

A concrete struct (not a trait) in `ansible-mesh-core`:

```rust
pub struct GraphDomain {
    adapter: Arc<dyn GraphAdapter>,
}
```

Every domain operation is a method on `GraphDomain` that composes `GraphAdapter`
primitives. No SQL. No backend coupling.

Representative translations:

| GraphStorage method | GraphDomain equivalent |
|---|---|
| `get_hotel(name)` | `get_node("hotel:{name}")` + deserialize |
| `list_guests(hotel, active_only)` | `list_nodes_by_kind("guest")` + filter |
| `upsert_role_incarnation(r)` | `upsert_node(role_incarnation:{agent}:{role})` |
| `upsert_agent_resource_grant(g)` | `upsert_node(resource_grant:{id})` + `upsert_edge(agent→grant)` |
| `get_session(id)` | `get_node("session:{id}")` + deserialize |

Adding a new entity type going forward: add a kind constant + serde + methods in
`GraphDomain`. No schema migration gating the abstraction.

### Layer 3 — Graph store instances

All graph stores are `GraphDomain` instances, each wrapping their own `GraphAdapter`:

| Store | Scope | Backing adapter |
|---|---|---|
| Hotel Context Graph | Hotel-scoped | `SqliteGraphAdapter` over CG db |
| Agent graph (graph-runner) | Agent/project-scoped | `SqliteGraphAdapter` over agent db |
| Training trace store | RL flywheel | `SqliteGraphAdapter` (or RocksDb) over trace db |

All stores speak the same data language: same `GraphNode`/`GraphEdge` types, same kind
constants, same serde contracts.

### Shared kind constants

Define a controlled vocabulary in `ansible-mesh-core/src/graph.rs` or a new
`graph_kinds.rs`:

```rust
pub const NODE_KIND_HOTEL: &str = "hotel";
pub const NODE_KIND_GUEST: &str = "guest";
pub const NODE_KIND_SESSION: &str = "session";
pub const NODE_KIND_ROLE_INCARNATION: &str = "role_incarnation";
pub const NODE_KIND_RULE: &str = "rule";
pub const NODE_KIND_RESOURCE_GRANT: &str = "resource_grant";
pub const NODE_KIND_ABSTRACT_TOOL: &str = "abstract_tool";
pub const NODE_KIND_ABSTRACT_SKILL: &str = "abstract_skill";
pub const NODE_KIND_WORKFLOW_SKILL: &str = "workflow_skill";
pub const NODE_KIND_TOOLSET_PROFILE: &str = "toolset_profile";
pub const EDGE_KIND_GUEST_OF: &str = "guest_of";
pub const EDGE_KIND_GRANT_TO: &str = "grant_to";
pub const EDGE_KIND_SESSION_OF: &str = "session_of";
```

These constants are the shared data language. Any graph store that reads a node with
`kind = NODE_KIND_GUEST` can deserialize it as a `GuestRecord`.

---

## Disposition

`proposed` — not yet implemented. Acceptance gates:

1. `GraphDomain` struct defined with at least two entity types migrated (proof of layer).
2. `SqliteGraphAdapter` extracted from `SqliteGraphStorage` implementing only `GraphAdapter`.
3. Hotel CG callers updated to `Arc<GraphDomain>`.

---

## Migration Path

The migration is seam-by-seam; no big-bang rewrite required.

1. **Extract `SqliteGraphAdapter`**: Peel the node/edge SQL out of `SqliteGraphStorage`
   into a new `SqliteGraphAdapter`. `SqliteGraphStorage` keeps its remaining domain methods
   intact — nothing breaks.

2. **Stand up `GraphDomain`**: Implement `GraphDomain::new(adapter)` and migrate 2–3
   entity types (e.g. `hotel`, `guest`, `session`) as a working proof. Existing
   `GraphStorage` callers are untouched.

3. **Migrate remaining entity types**: Translate each `GraphStorage` method to a
   `GraphDomain` method. SQL for that entity type moves into the adapter; domain logic
   moves into `GraphDomain`.

4. **Swap callers**: Replace `Arc<dyn GraphStorage>` with `Arc<GraphDomain>` at each
   call site. `GraphStorage` becomes a compatibility shim (or is deleted once callers
   are clean).

5. **Wire new stores**: Hotel CG, agent graph (graph-runner), and training trace store
   each get a `GraphDomain` instance over their own adapter. Shared kind constants
   enforce a consistent data language across all three.

Transitional invariant: `GraphStorage` may remain as a shim forwarding to `GraphDomain`
while migration proceeds — no forced cutover.

---

## Seams

| Seam slug | Scope | Deliverable |
|---|---|---|
| `graph-domain-layer` | `ansible-mesh-core` | Define `GraphDomain` struct; migrate `hotel`, `guest`, `session` entity types as proof |
| `graph-adapter-migration` | `ansible-mesh-core` | Migrate remaining `GraphStorage` methods to `GraphDomain`; extract `SqliteGraphAdapter` |
| `graph-store-instances` | `aiua`, `graph-runner`, training | Wire hotel CG, agent graph, trace store onto `GraphDomain`; delete `GraphStorage` shim |

---

## Non-Goals

- This proposal does not change the node/edge wire format or introduce schema versioning.
- It does not merge the three graph store instances into one database — physical separation
  by scope is intentional and preserved.
- It does not replace Muninn (semantic recall) or add new query capabilities; `GraphAdapter`
  primitives are unchanged.
