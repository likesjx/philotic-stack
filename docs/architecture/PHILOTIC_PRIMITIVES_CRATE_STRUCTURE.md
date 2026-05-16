---
title: Philotic Primitives Crate Structure
doc_type: proposal
domain: runtime-sessions
status: accepted-current-slice
last_updated: 2026-04-09
tags:
- architecture
- crate-structure
- primitives
- technical-debt
related_docs:
- ARCHITECTURE.md
- ARCHITECTURE_STATUS.md
task_refs:
- docs/task.md
tracks_domains:
- runtime-sessions
- tooling-execution
- memory-context
---

# Philotic Primitives Crate Structure Proposal

## Goal

Split `ansible-mesh-core` into a small set of primitive crates that match the
repo's actual runtime boundaries instead of keeping every shared type, trait,
envelope, and storage helper in one catch-all crate.

The split should make the compiler enforce the same boundary that the runtime
already implies:

- hotel orchestration belongs with `aiua`
- agent-loop state belongs with `philote`
- model/provider execution belongs with `model-router`
- tool execution belongs with `tool-runner`
- graph/memory/storage primitives should no longer be a monolith

## Core Recommendation

Phase out the `ansible-mesh-core` monolith and replace it with a structured set
of primitive crates. These crates should own the stateless data envelopes, IPC
message variants, and shared traits required by each specific subsystem, which
prevents dependency creep and keeps boundary ownership honest at compile time.

The split should follow dependency depth, not aesthetic preference:

1. `philotic-primitives-mesh` / `philotic-primitives-core`
2. `philotic-primitives-data`
3. `philotic-primitives-hotel`
4. `philotic-primitives-agent`
5. `philotic-primitives-model`
6. `philotic-primitives-tool`

If a type is shared across multiple crates, it belongs in the lowest crate that
can own it without dragging in unrelated runtime behavior.

**Proposed Crate Splitting:**

1. **`philotic-primitives-mesh` (or `philotic-primitives-core`)**
   - The absolute bottom layer.
   - Contains: `EventEnvelope`, `EventId`, base `TerminalErrorCode`, `BeaconMessage`, and cryptographic primitives/authz.

2. **`philotic-primitives-hotel`**
   - Area: Hotel daemon (aiua) and orchestration.
   - Contains: `GuestRecord`, `HotelRecord`, `NodeCapabilities`, `Materializer` trait, capability registry types, loop sync and supervision events.

3. **`philotic-primitives-agent`**
   - Area: Persona/agent loop (philote).
   - Contains: `AgentIdentityRecord`, `RoleIncarnationRecord`, `RuleRecord`, `SessionRecord`, `SessionParticipantRecord`, `SessionTurnRecord`, agent-specific execution constraints.

4. **`philotic-primitives-model`**
   - Area: Model execution and reasoning (model-router).
   - Contains: `ModelManagerInvoker`, `request_class` definitions, structured model envelopes (`text.generate`, `voice.synthesize`), model capability advertisements.

5. **`philotic-primitives-tool`**
   - Area: Tool and skill execution (tool-runner).
   - Contains: `AbstractToolRecord`, `AbstractSkillRecord`, `WorkflowSkillRecord`, `ToolsetProfileRecord`, tool invocation envelopes.

6. **`philotic-primitives-data`**
   - Area: Memory, Context Graph backend, storage interfaces.
   - Contains: Context/memory projection types, `GraphStorage`, `EventStorage`, `CursorStorage`, `Apartment` abstractions, and database adapter schemas.

## Disposition

`accepted-current-slice`

## Repo Truth Right Now

- `ansible-mesh-core` still owns the shared primitives surface.
- `aiua`, `philote`, `model-router`, and `tool-runner` already imply the
  runtime ownership boundaries the split should preserve.
- the current refactor pressure is to separate ownership boundaries without
  inventing a second authority for graph, session, or model state.

## Current Slice

Turn the proposal into a dependency map and extraction plan:

- inventory the current `ansible-mesh-core` modules against the target crates
- identify the first extraction boundary that can compile cleanly on its own
- define the dependency order for the remaining crates
- keep the runtime code unchanged until the first extracted interface is stable

Linked task surface: [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
