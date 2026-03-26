---
title: "GraphStorage → GraphDomain Migration Tracker"
doc_type: task
domain: graph-domain-layer
status: in-progress
last_updated: 2026-03-26
tags:
  - graph-domain
  - migration
  - cleanup
related_proposals:
  - GRAPH_LAYER_UNIFICATION_PROPOSAL.md
---

# GraphStorage → GraphDomain Migration Tracker

## Goal

Eliminate all direct `GraphStorage` trait usage in favor of the `GraphDomain` middle
layer. `GraphDomain` provides entity-typed methods (hotel, guest, agent, session, etc.)
that enforce naming conventions and reduce raw SQL surface area.

## Current State (2026-03-26)

| Location | GraphStorage refs | GraphDomain refs | Notes |
|---|---|---|---|
| `aiua/src/service/ipc.rs` | ~55 (mostly tests) | 96 | Production code mostly migrated; tests still use SqliteGraphStorage directly |
| `aiua/src/main.rs` | 9 | 26 | A few startup/bootstrap refs remain |
| `aiua/src/auth.rs` | 2 | 0 | Still opens SqliteGraphStorage directly for OAuth |
| `aiua/src/memory.rs` | 3 | 1 | Test helper opens raw storage |
| `agent-graph-runner/` | 17 | 0 | Uses AgentGraphStorage trait (separate) — not part of this migration |
| `ansible-mesh-core/` | trait def + sqlite impl | domain.rs (1309 LOC) | GraphStorage trait is the foundation; GraphDomain wraps it |

**Total remaining GraphStorage refs: ~122** (excluding trait definition and agent-graph-runner)

## Migration Strategy

### Phase 1: Test infrastructure (S — single session)
Most remaining refs are test setup: `SqliteGraphStorage::open(":memory:")` + `GraphDomain::new(Arc::new(storage))`.
Create a `test_helpers::open_test_domain()` function that returns `(SqliteGraphStorage, GraphDomain)`.
Replace all test setup blocks.

### Phase 2: Auth bootstrap (S)
`auth.rs` opens `SqliteGraphStorage` directly for OAuth flows. Route through the existing
`GraphDomain` instance passed from main.

### Phase 3: IPC production code (S)
3 remaining error log messages reference "GraphStorage" in string literals. Update to "GraphDomain".

### Phase 4: Evaluate trait removal (M)
Once all callers go through `GraphDomain`, evaluate whether `GraphStorage` trait can become
`pub(crate)` or be inlined into `GraphDomain`. This is the final unification step from
GRAPH_LAYER_UNIFICATION_PROPOSAL.

## Not In Scope

- `AgentGraphStorage` trait in `agent-graph-runner/` — this is a separate per-agent graph, not the hotel context graph.
- `GraphStorage` trait definition itself — it stays as the underlying contract until Phase 4.
