---
title: Graph Datasource Proposal
doc_type: proposal
domain: memory-context
status: accepted-current-slice
last_updated: 2026-04-18
tags:
- datasource
- graph
- memory
- work-product
related_docs:
- ARCHITECTURE_STATUS.md
- MEMORY_LAYERING_AND_WORK_PRODUCT_SPLIT_PROPOSAL.md
- GRAPH_DATASOURCE_PHILOTE_PROPOSAL.md
task_refs:
- docs/task.md
disposition: accepted for current slice
---

# Graph Datasource Proposal

## Goal

Elevate the semantic memory subgraph capability from a generic `tool-runner` into a dedicated resource type called `graph-datasource` (or `datasource` fundamentally). This base crate will enable agents to dynamically create, own, share, and update distinct subgraphs with isolated lifecycles.

## Core Recommendation

Land this seam in two honest steps instead of pretending the rename and the runtime extraction are one harmless move:

1. introduce `datasource-core` as a shared crate with the common task/provider/runtime contracts
2. add a placeholder `graph-datasource` guest shell that can host providers later without yet renaming or deleting `graph-runner` / `agent-graph-runner`
3. keep the actual graph and agent datasource migration as a follow-on seam once current `develop` divergence is resolved on purpose

## Disposition

Accepted for the current slice: the replay/merge branch yielded one small reusable seam we can land cleanly on current `develop`, while the broader runner migration still conflicts with too much unrelated evolution to call it a safe replay.

## Current Slice

- `datasource-core` is now replayed as a real shared contract crate instead of a placeholder `add(2, 2)` scaffold.
- `graph-datasource` now exists as a deterministic guest shell with zero providers registered, making the unfinished state explicit instead of implying the migration already happened.
- `graph-runner` and `agent-graph-runner` remain the current runtime truth until the dedicated migration seam lands.

## 1. The Core Vision
The agent should be able to:
1. **Create and Own Partitions:** Act as the sovereign authority over a semantic partition (subgraph), mapping its local scratchpad, memories, or world models.
2. **Share Partitions:** Hand off a subgraph pointer to another agent or process without copying all the data.
3. **Update Partitions:** Mutate its owned partitions without affecting the global hotel-level context graph unless explicitly merged.
4. **Target Interface:** The underlying graph engine (SQLite, PostgreSQL, Neo4j, etc.) is abstracted behind an **Apache AGE (Cypher)** compliant interface.
5. **Distinct Resource Lifecycle:** Handled via a new separate resource type (`datasource`), instead of a vanilla `tool-runner`.

## 2. Transitioning to an AGE-Compliant Interface (Cypher)

To make the implementation agnostic, we must pivot the "Agent facing" RPC surface to accept universal Cypher queries.

### Proposed Interface Changes:
- **`graph.query` (New):** Executes Cypher directly against a partition.
- **`graph.create` (Modified):** Provisions a logical Cypher space.

**Implementation Abstraction:**
The `GraphStore` trait should focus around executing generic Cypher queries so the backing datastore (SQLite initially, then Postgres+AGE) becomes irrelevant.

## 3. Subgraph Ownership: Distinct Graph IDs + Tag-Based Partitions
We will use a hybrid approach heavily aligning with Apache AGE:
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

2. **Seam: `ast-cypher-parser`**
   - Introduce a Cypher parser (or lightweight binding) inside the `graph-datasource`.
   - Initial SQLite transpiler that translates `MATCH` / `CREATE` into existing traversal filters.

3. **Seam: `graph-runner-migration`**
   - Evolve the existing `graph-runner` / `agent-graph-runner` stack to the new datasource structure, ensuring zero data loss and moving basic tools to `graph.query` projection.

## 5. Summary of the Target Flow
1. **Materialization:** Hotel boots `graph-datasource` guest process.
2. **Context Partition Creation:** Agent establishes a partition (`graph.create`).
3. **Collaboration:** Agent delegates to a subagent, sharing the partition ID and granting visibility tags.
4. **Querying:** Subagent queries the partition natively using Cypher syntax via `graph.query` tool.
