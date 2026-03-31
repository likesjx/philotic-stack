---
title: Memory Relation Lifecycle Whitepaper
doc_type: proposal
domain: memory-context
status: proposed
last_updated: 2026-03-31
tags:
- memory
- relations
- sleep
- muninn
- context
- active-seam
related_docs:
- ARCHITECTURE_STATUS.md
- MEMORY_ENGINE_ABSTRACTION_PROPOSAL.md
- MUNINN_MEMORY_PROTOCOL_PROPOSAL.md
- PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md
- MODEL_CONTROLLER_PROPOSAL.md
- HEURISTIC_MIND_AND_CONTEXT_PAPER.md
task_refs:
- docs/task.md
proposal_id: memory-relation-lifecycle-whitepaper
implements: []
implemented_by: []
active_seams:
- memory-formation-lifecycle
- provisional-relation-layer
- sleep-consolidation-heuristics
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Memory Relation Lifecycle Whitepaper

## Goal

Capture a preliminary architecture for Philotic memory that treats memory as a living relational network rather than a bag of isolated notes or a thin retrieval cache.

This paper is intentionally provisional.

It is meant to preserve the topology of the current design insight before the details harden into a narrower implementation proposal.

## Disposition

`proposed`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Executive Summary

The core claim is simple:

memory value emerges from relational topology, not from isolated note quality.

That means Philotic should not treat memory as:

- transcript dumping
- one-shot perfect summaries
- a glorified vector store
- or a second source of runtime truth

Instead, the system should separate:

1. runtime truth
2. memory formation
3. memory storage and recall
4. relational consolidation over time

Muninn is especially valuable here not just because it can store and retrieve memories, but because it can shape importance heuristically over time:

- strengthening some paths
- weakening others
- merging duplicates
- surfacing contradictions
- pruning low-value residue

The resulting architecture is not “the agent remembers everything perfectly.”

It is:

- admit candidate memories generously and atomically
- form provisional relation structure in context
- let a sleep/consolidation cycle reorganize the memory network
- project only the currently useful relational structure back into cognition

## Core Recommendation

Philotic should adopt a memory architecture built from four distinct concerns:

1. **runtime authority**
2. **memory formation**
3. **memory controller**
4. **utility inference**

These should not collapse into one system just because they all touch context.

### Runtime Authority

Philotic should remain the owner of:

- session truth
- conversation turn state
- cognitive step state
- active role/incarnation
- bindings and approvals
- local working memory
- final policy decisions about what enters durable memory

### Memory Formation

A separate memory-formation path should determine what candidate memories actually say.

This path may use:

- extraction
- summarization
- classification
- contradiction detection
- salience heuristics
- model assistance

But it should not be identical to the main conversational reasoning path.

### Memory Controller

Muninn should act as a memory controller, not as the owner of runtime truth.

It should own:

- store
- search
- provenance
- durable relation management
- reinforcement/weakening
- consolidation and sleep-time reorganization

### Utility Inference

Embeddings and small local function-style models should be treated as utility inference, not cognitive ceremony.

They should support:

- memory recall
- memory write-time indexing
- reranking
- clustering
- extraction/classification helpers

They may be reachable through the broader model taxonomy, but they should also have a direct hotel-local path.

## Design Principle

Atomic memories are for write-time clarity.

Networked memory is for long-term intelligence.

This avoids two bad extremes:

- giant blobs that cannot be recombined
- tiny isolated notes that never become meaning

## The Planes

Philotic memory now wants four interacting planes.

### 1. Runtime Authority Plane

This plane owns structural truth about what is happening now.

Examples:

- who is active in the session
- which role/incarnation is active
- what bindings and approvals are in force
- what the current conversation turn contains
- what the current working memory says

This plane is authoritative.

### 2. Cognitive Plane

This is the active reasoning path for the current conversation turn.

It consumes:

- context projection
- role posture
- memory projection
- tools and skills
- active turn content

This is where the model helps produce the user-facing response.

### 3. Memory Plane

This is Muninn’s home.

It stores:

- atomic memories
- durable relations
- reinforcement signals
- contradiction/supersession signals
- consolidated clusters

This plane is durable, heuristic, and reorganized over time.

### 4. Utility Inference Plane

This plane provides:

- embeddings
- reranking
- classification
- extraction
- other low-latency local model operations

It should usually be hotel-local and thin.

## Request Classes

The model taxonomy should distinguish capability from cognitive weight.

- `capability` answers: what work is requested?
- `request_class` answers: what kind of execution contract does this work require?

Initial request classes:

- `cognitive`
- `transform`
- `synthesis`
- `embedding`

This matters because two calls can both be `text.generate` while having very different routing and memory implications.

Examples:

- a full agent reasoning turn with tools, skills, and layered context
- a tiny utility prompt that only rewrites a sentence

Those are not the same species of work.

### Current Intuition

- `cognitive` calls may use context, context projection, role posture, tools, and skills
- `transform` calls are narrow task-local conversions such as transcription or media analysis
- `synthesis` calls produce artifacts such as speech
- `embedding` calls support memory infrastructure and retrieval

## Memory Formation

The question “who decides what becomes memory?” should not be answered by choosing only the runtime or only the model.

The healthier split is:

- the model proposes
- the runtime decides

More precisely:

- the runtime decides whether a checkpoint is memory-worthy
- a memory-formation path drafts candidate memory content
- the runtime decides whether the candidate is admissible and where it belongs
- Muninn stores and organizes the resulting memory object

This preserves nuance without letting the model become the sovereign author of autobiographical fan fiction.

### Memory Formation Inputs

Candidate memory formation may consider:

- incoming turn content
- structured context projection
- model response
- tool results
- approvals/denials
- contradictions with existing memory
- relationship hints already projected in the turn

## Provisional Relation Layer

Philotic likely needs a relationship layer for structured recall, but that does not imply another permanent graph above Muninn.

The right move is a **provisional relation layer** first.

This layer exists in context before it exists durably in memory.

### Why This Matters

Some relation structure is useful immediately even when it is not yet durable truth.

Examples:

- this decision appears to contradict an older assumption
- this user and this project are jointly salient right now
- this role is active for this session
- these retrieved memories cluster around this seam

The system should be able to think relationally before it commits relationally.

### Relation Sources

The provisional relation layer can be assembled from:

- Philotic structural truth
- Muninn recall results
- current turn content
- retrieval-time inference
- response-time emergence

### Relation Lifetimes

Different relations should live for different durations:

- `turn-local`
- `session-local`
- `candidate-durable`
- `durable`

### Relation Rigidity

Most relations should start soft.

Useful properties:

- source
- confidence
- authority
- lifetime/TTL
- promotion eligibility

Important rule:

structure early, rigidity late.

## Memory Lifecycle

Memory should not be treated as a single write event.

It should have a lifecycle.

### 1. Candidate

The system forms atomic candidate memories from the live interaction.

Candidates may be:

- decision
- preference
- unresolved question
- fact claim
- contradiction
- relational signal
- task seam

### 2. Remembered

Candidates that survive admission are stored with lightweight initial typing and confidence.

At this stage, they may still be weakly held.

### 3. Reinforced

Repeated recall, reuse, confirmation, or relational recurrence strengthens them.

### 4. Consolidated

Sleep/consolidation may:

- merge duplicates
- strengthen central edges
- clarify contradiction/supersession
- cluster related memories

### 5. Weakened

Memories or edges that do not recur may lose salience.

### 6. Archived or Forgotten

Some low-value residue may be archived, suppressed, or eventually forgotten.

The key is that importance is allowed to emerge over time instead of being declared perfectly at birth.

## Sleep and Consolidation

Sleep is not just compaction.

It is network reorganization.

Possible sleep-time operations:

- strengthen repeated motifs
- merge near-duplicate memories
- weaken weakly supported edges
- break false associations
- promote recurring signals into stronger memory
- surface contradictions
- archive low-value residue

Important constraint:

sleep should reorganize memory structure and salience, not mutate canonical runtime truth.

## Embeddings

Embeddings are not exotic model calls.

They are memory infrastructure.

That matters because memory will likely need embeddings for both:

- remembering
- recall

And probably also for:

- reranking
- dedupe
- clustering
- semantic bridging

### Recommended Placement

Embeddings should remain part of the broader inference taxonomy, but should not require the full cognitive path.

Recommended default:

- direct hotel-local utility inference service

Still allowed:

- optional router-mediated embedding path

This preserves one shared language without forcing every embedding request through reply routing and conversational semantics.

## Muninn’s Role

Muninn should not be treated as the native owner of all context layers.

It should be treated as a first-class memory controller that:

- receives formed memories
- stores atomic memory objects
- organizes durable relations
- performs recall
- supports explanation/provenance
- runs heuristic consolidation over time

Philotic should still own:

- session truth
- conversation turn truth
- cognitive step truth
- context projection assembly
- role and identity authority

## What Sits On Top Of Muninn

Not another permanent graph layer.

What sits on top is a memory orchestration layer that handles:

- admission policy
- memory formation
- projection into context
- promotion decisions
- sleep scheduling
- authority gating

This keeps the architecture from creating a graph of graphs because one graph apparently did not provide enough opportunities for metaphysical confusion.

## Open Questions

These questions still need deliberate design before a narrower implementation proposal:

1. What exact checkpoints should trigger memory formation?
2. Which relation types belong to Philotic structural truth versus Muninn heuristic memory versus provisional context only?
3. How aggressive should candidate capture be by default?
4. Which heuristics should control promotion, weakening, and forgetting?
5. How should contradiction and supersession be modeled across provisional and durable layers?
6. How much of memory formation should use small local models versus heavier cognitive model calls?
7. When should embedding work go direct to local utility inference versus through router mediation?
8. What inspection/debug surface is required so operators can understand why a memory or relation exists?

## First Honest Follow-On Proposal

The next narrower proposal should likely define:

1. memory formation checkpoints and payloads
2. provisional relation-layer states and lifetimes
3. sleep/consolidation operations and guardrails
4. embedding service contract and direct hotel path
5. promotion and authority rules between Philotic and Muninn

## Current Slice

This paper captures the current design direction after defining:

- context layers and `conversation turn` / `cognitive step`
- request classes for cognitive, transform, synthesis, and embedding
- a direct local utility-inference intuition for embeddings
- Muninn as a memory controller rather than the owner of runtime truth
- a provisional relation layer that can exist in context before it becomes durable memory

It does not yet claim that this full lifecycle is implemented.

That would be a charming lie.
