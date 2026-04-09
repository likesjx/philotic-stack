---
title: Philotic Primitives Crate Structure
doc_type: proposal
domain: runtime-sessions
status: proposed
last_updated: 2026-04-09
tags:
- architecture
- crate-structure
- primitives
- technical-debt
related_docs:
- ARCHITECTURE.md
task_refs:
- docs/task.md
tracks_domains:
- runtime-sessions
- tooling-execution
- memory-context
---

# Philotic Primitives Crate Structure Proposal

## Goal

Ensure our crate architecture follows and enforces our distinct runtime boundaries.
Currently, `ansible-mesh-core` acts as a monolithic catch-all for all types, envelopes, storage traits, and mesh operations. 

We need to decouple these primitives into distinct areas of concern tied to our functionality primitives: **hotel (aiua)**, **agent (philote)**, **model controller**, **tool runner**, and **data runner**.

## Core Recommendation

Phase out the `ansible-mesh-core` monolith and replace it with a structured set of primitive crates. These crates will contain the stateless data envelopes, IPC message variants, and shared traits required by each specific subsystem, preventing dependency creep and enforcing architectural boundaries at compile time.

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

`proposed`

## Current Slice

- Drafted the initial proposal to break down `ansible-mesh-core`.
- Waiting for operator alignment before scaffolding the inner crates and re-wiring the workspaces.
