---
title: Graph Datasource Proposal
doc_type: proposal
domain: memory-context
status: accepted-current-slice
last_updated: 2026-06-02
tags:
- datasource
- graph
- memory
- work-product
related_docs:
- ARCHITECTURE_STATUS.md
- LIFE_GRAPH_OS_PROPOSAL.md
- MEMORY_LAYERING_AND_WORK_PRODUCT_SPLIT_PROPOSAL.md
- GRAPH_DATASOURCE_PHILOTE_PROPOSAL.md
task_refs:
- docs/task.md
disposition: accepted-current-slice
---

# Graph Datasource Proposal

## Goal

Elevate the semantic memory subgraph capability from a generic `tool-runner` into a dedicated resource type called `graph-datasource` (or `datasource` fundamentally). This base crate will enable agents to dynamically create, own, share, and update distinct subgraphs with isolated lifecycles.

## Core Recommendation

Land this seam in honest steps instead of pretending the rename, runtime extraction,
and Cypher story are one harmless move:

1. introduce `datasource-core` as a shared crate with the common task/provider/runtime contracts
2. add a placeholder `graph-datasource` guest shell that can host providers later without yet renaming or deleting `graph-runner` / `agent-graph-runner`
3. keep the actual graph and agent datasource migration as a follow-on seam once current `develop` divergence is resolved on purpose
4. treat the current SQLite regex transpiler as a bridge only, then move real Cypher execution to a provider-backed database
5. focus the next implementation slice on Memgraph as the centralized graph authority candidate for always-on, multi-hotel graph operations
6. keep Kuzu as a deferred embedded-provider experiment until its Rust binding/linker and maintenance story are clearer

## Disposition

Accepted for the current slice: graph-datasource exists, but the Cypher backend
direction is revised. SQLite-backed regex translation is transitional runtime
truth, not the target. The next backend slice should focus on Memgraph via Bolt
as the central graph provider; Kuzu remains a deferred embedded-provider spike.

## Current Slice

- `datasource-core` is now replayed as a real shared contract crate instead of a placeholder `add(2, 2)` scaffold.
- `graph-datasource` now exists as a deterministic guest shell with zero providers registered, making the unfinished state explicit instead of implying the migration already happened.
- `graph-runner` and `agent-graph-runner` remain the current runtime truth until the dedicated migration seam lands.
- `SqliteCypherProvider` remains a bridge for installed graph.query behavior, but it must not be expanded into a homegrown Cypher engine.
- Memgraph is accepted as the preferred current implementation focus for centralized Cypher graph authority.
- Life Graph OS is a proposed consumer of this provider boundary: it should see `graph.query` and named retrieval strategies, not Bolt hostnames, ports, or database-specific operational detail.
- Kuzu remains interesting for embedded/local graphs but is blocked by Rust binding/linker and upstream maintenance risk.

## 1. The Core Vision
The agent should be able to:
1. **Create and Own Partitions:** Act as the sovereign authority over a semantic partition (subgraph), mapping its local scratchpad, memories, or world models.
2. **Share Partitions:** Hand off a subgraph pointer to another agent or process without copying all the data.
3. **Update Partitions:** Mutate its owned partitions without affecting the global hotel-level context graph unless explicitly merged.
4. **Target Interface:** The agent-facing surface is Cypher-first. The underlying graph engine is abstracted behind a provider boundary that can run the current SQLite bridge, a centralized Memgraph backend, or a future embedded backend.
5. **Distinct Resource Lifecycle:** Handled via a new separate resource type (`datasource`), instead of a vanilla `tool-runner`.

## 2. Transitioning to a Real Cypher Interface

To make the implementation agnostic, we must pivot the agent-facing RPC surface
to accept Cypher queries without making agents memorize a large set of graph
CRUD tools. Structured tools can remain as deterministic wrappers, but
`graph.query` should be the primary cognitive interface.

### Proposed Interface Changes:
- **`graph.query` (New):** Executes Cypher directly against a partition.
- **`graph.create` (Modified):** Provisions a logical Cypher space.
- **`graph.schema` / `graph.explain` / `graph.validate` (Future):** Gives
  agents enough graph vocabulary, query-plan feedback, and safety checks to
  write Cypher reliably without turning every graph operation into a separate
  tool.

**Implementation Abstraction:**
The `GraphStore` trait should focus around executing Cypher queries and
returning graph-shaped results so the backing datastore is a deployment choice,
not an agent-facing contract.

### Backend Ladder

1. **SQLite bridge (current runtime truth)**
   - Keep `SqliteCypherProvider` for compatibility and fast local rollout.
   - Limit it to a small, explicit subset while the real backend lands.
   - Do not add broad regex parsing for chained `MATCH`, `MERGE`, variable
     binding, path traversal, or query planning. That would be a graph database
     by accident.

2. **Memgraph centralized backend (current implementation focus)**
   - Run Memgraph on `vps-jane` in Docker/Compose as the always-on graph
     authority for cross-hotel Cypher operations.
   - Connect graph-datasource through Bolt on port `7687` via the provider
     boundary, not through agent-visible host/port knowledge.
   - Keep Memgraph service config, volume paths, and credentials in deployment
     config/secrets so agents keep seeing `graph.query`, not infrastructure.
   - The first Memgraph slice should prove `CREATE`, `MATCH`, `MERGE`,
     relationship creation from matched variables, and bounded `RETURN`
     behavior against Beacon-style graph writes.

3. **Kuzu embedded backend (deferred local-provider experiment)**
   - Kuzu keeps SQLite-like deployment ergonomics while providing a native
     property graph and Cypher query model.
   - This could still fit local hotel graphs, agent-owned partitions, and
     portable development environments.
   - Current caveat: the upstream Kuzu repository is archived and Homebrew marks
     the formula deprecated. Kuzu remains useful for the transition spike, but
     production adoption needs an explicit maintenance/fork plan.
   - Current implementation caveat: the Kuzu Rust provider type-checks behind a
     feature flag, but feature tests fail at final link on macOS Tahoe/Rust 1.94
     with missing Kuzu cxxbridge symbols.

## 3. Subgraph Ownership: Distinct Graph IDs + Tag-Based Partitions
We will use a hybrid approach aligned with Cypher-native property graph
semantics:
- **Distinct Graph Namespaces:** A partition maps directly to a distinct `graph_id` (representing a discrete schema namespace). 
- **Tag-Based Visibility Control:** Nodes and edges inside and across partitioned graphs continue to utilize node-level access control tags for fine-grained sharing (`identity:agent_foo`).
- **Resource Management:** Partitions are managed by the new `graph-datasource` resource block, treated as separate partitions natively.

## 4. Name Change and Crate Separation (Datasource implementation)
To achieve this, we will rename the targeted `tool-runner` architecture to a **DataSource** resource model. We will:
- Introduce a base `datasource` crate.
- Build `graph-datasource` off this crate.
- While it still exposes an interfacing surface that is "tool-based" (i.e. agents call tools to query the datasource), it is managed independently from plain arbitrary scripts/commands during Hotel runtime provisioning (e.g. `start_datasource` / distinct Guest processes).

## 4. Work Breakdown & Implementation Seams

1. **Seam: `datasource-core`**
   - Extract/build the base `datasource` crate.
   - Separate the runtime provisioning for `graph-datasource`.

2. **Seam: `central-graph-provider`**
   - Add a provider-backed Cypher execution path, starting with Memgraph over
     Bolt.
   - Deploy Memgraph on `vps-jane` in Docker/Compose with persistent volume,
     backup procedure, and mesh-visible endpoint/config.
   - Define provider contract tests around Beacon-style graph writes:
     `MATCH`, `MERGE`, relationship creation, and graph-shaped `RETURN`.

3. **Seam: `embedded-cypher-provider`**
   - Keep Kuzu as a deferred embedded-provider option after its binding and
     maintenance risks are resolved.
   - Keep the SQLite transpiler as a compatibility bridge only.
   - Define provider contract tests around Beacon-style graph writes:
     `MATCH`, `MERGE`, relationship creation, and graph-shaped `RETURN`.

4. **Seam: `graph-runner-migration`**
   - Evolve the existing `graph-runner` / `agent-graph-runner` stack to the new datasource structure, ensuring zero data loss and moving basic tools to `graph.query` projection.

## 5. Summary of the Target Flow
1. **Materialization:** Hotel boots `graph-datasource` guest process.
2. **Context Partition Creation:** Agent establishes a partition (`graph.create`).
3. **Collaboration:** Agent delegates to a subagent, sharing the partition ID and granting visibility tags.
4. **Querying:** Subagent queries the partition natively using Cypher syntax via `graph.query` tool.
