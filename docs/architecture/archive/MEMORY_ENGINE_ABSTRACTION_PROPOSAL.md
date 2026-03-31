---
title: Memory Engine Abstraction Proposal
doc_type: proposal
domain: memory-context
status: proposed
last_updated: 2026-03-31
tags:
- memory
- engine
- abstraction
- muninn
- active-seam
related_docs:
- ARCHITECTURE_STATUS.md
- PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md
- MUNINN_MEMORY_PROTOCOL_PROPOSAL.md
- AGENT_PLUGIN_HOOKS_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: memory-engine-abstraction
implements: []
implemented_by: []
active_seams:
- memory-engine-contract
- graph-muninn-memory-dual-path
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Memory Engine Abstraction Proposal

## Goal

Define a real abstraction boundary for memory handling so Philotic can support multiple memory backends and strategies without making one current implementation the accidental final ontology.

## Disposition

`proposed`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md) and [MUNINN_MEMORY_PROTOCOL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MUNINN_MEMORY_PROTOCOL_PROPOSAL.md).

## Core Recommendation

Philotic should separate:

1. **memory interface**
2. **memory policy**
3. **memory backend**

The system should reason about memory through a stable interface, while allowing different implementations such as:

- graph-native memory
- Muninn-backed heuristic memory
- imported legacy memory seeds
- future vector/embedding stores
- local fallback memory engines

## Recommended Boundary

The abstraction should own:

- lookup/search
- store/write-back
- provenance
- confidence or retrieval metadata
- budget and recency policy hooks

The cognitive loop should not care whether the memory came from Muninn, SQLite, ONNX embeddings, or markdown amber from a former life.

## Why This Matters

Right now memory work risks collapsing into:

- one provider
- one prompt pattern
- one storage representation

That is expedient, but it makes migration and experimentation much harder than they need to be.

## First Slice Recommendation

Define the first memory engine contract with:

- `search`
- `store`
- `explain/provenance`

Then adapt the current Muninn path and one graph-native path behind the same interface.
