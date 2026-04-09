---
title: Philotic Agent Loop Proposal
doc_type: historical
domain: runtime-sessions
status: historical
last_updated: 2026-04-08
tags:
- archived
- proposal
- agent-loop
related_docs:
- ARCHITECTURE_STATUS.md
---

# Philotic Agent Loop Proposal

## Goal

Define the next-stage Philotic agent loop as a dedicated project rather than letting it blur into the already-completed session and checkpoint work.

This loop should be:

- Pi-shaped in core execution semantics
- Philotic-owned in session/durability/orchestration
- approval-aware
- tool-capable
- checkpointed at meaningful boundaries

## Disposition

Accepted for the current slice and partially implemented.

The loop now has:

- explicit turn phases
- structured `respond` / `tool_call` / `request_approval` actions
- approval interrupts and resume behavior
- routed tool execution

Still pending in [task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md):

- bounded multi-iteration loop completion
- richer context building and compaction policy
- provider boundary refinement

## Why This Is A Separate Project

We have completed the foundational runtime work:

- canonical session ownership in the graph
- session/turn/event persistence
- agent-local working turn state
- graph-first recovery snapshots
- per-session checkpoints
- deterministic slash-command short-circuiting
- binary smoke coverage using `/ping`

That is enough to support the next phase, but it is not the loop itself.

The actual bounded agent loop is now substantial enough to deserve its own proposal, spec, and implementation track.

## Design Inputs

The best combined design basis is:

- **Pi / `pi-agent-core`**
  - the cleanest direct ancestor of OpenClaw/ZeroClaw
  - provides the turn-engine skeleton
- **Anthropic / Claude Agent SDK**
  - strong streaming and tool-interaction patterns
- **OpenAI Responses / Agents SDK**
  - strong item/action-oriented interaction model
- **LangGraph**
  - strongest checkpoint/interrupt durability model

## Core Recommendation

Philotic should implement a **Pi-style turn engine inside `philote`**, but wrap it in **Philotic session durability and turn state management**.

That means:

- `philote` owns the in-turn loop
- `aiua` owns canonical session and turn durability
- checkpoints happen between super-steps
- approvals are real interrupts
- tools/results are first-class records, not just text

## What We Keep From Pi

Pi gets the core shape right:

- internal `AgentMessage` model
- `transformContext` before each model call
- provider conversion boundary
- streaming/eventful loop
- tool execution after assistant output
- repeat until no more tool calls or follow-up work

That should remain the heart of the Philotic loop.

## What Philotic Adds

Pi is an in-memory turn engine.
Philotic needs a durable, cross-component execution model.

So Philotic adds:

- canonical graph-backed session/turn state
- checkpointing between super-steps
- resumable approval interrupts
- stable `session_id` / `turn_id`
- component-level event timeline
- deterministic slash-command short-circuit path
- effective session bindings:
  - toolset
  - skillset
  - model-controller
  - workspace
  - policy

## Proposed Loop Shape

### 1. Intake

- accept inbound task
- resolve `session_id` / `turn_id`
- detect slash-command short-circuit
- load canonical session snapshot

### 2. Build Working Context

- reconstruct working turn state from:
  - recent turn records
  - session summary
  - apartment recovery checkpoint
  - effective bindings

- run `transformContext(...)`

### 3. Model Step

Ask the model for the next step, not merely the final answer.

Allowed next-step categories:

- `respond`
- `tool_call`
- `request_approval`
- `handoff`
- `fail`

### 4. Execute Super-Step

- `respond`
  - finalize turn
  - checkpoint
  - emit final reply
- `tool_call`
  - validate tool access
  - checkpoint intent
  - execute tool
  - append structured tool result
  - checkpoint
  - continue loop
- `request_approval`
  - checkpoint
  - persist interrupt state
  - return control to runtime
- `handoff`
  - emit structured cross-component step
  - checkpoint
- `fail`
  - record failure
  - checkpoint

### 5. Bound and End

- enforce iteration cap
- enforce tool and policy limits
- finish with `completed` or `failed`

## Proposed Turn State Model

Recommended durable turn states:

- `queued`
- `loading_context`
- `thinking`
- `waiting_tool`
- `waiting_approval`
- `resuming`
- `completed`
- `failed`

These states should be canonical at the session layer, not only implied inside the agent.

## Proposed Internal Action Model

Recommended action/result records:

- `user_message`
- `assistant_reasoning_summary`
- `assistant_response`
- `tool_request`
- `tool_result`
- `approval_request`
- `approval_resolution`
- `handoff_request`
- `failure`

These should be structured, append-only, and checkpoint-friendly.

## Provider Boundary

Philotic should preserve Pi's model:

- internal neutral agent messages
- explicit provider conversion at the boundary

Recommended boundary hooks:

- `transform_context(session_snapshot, working_state) -> AgentMessage[]`
- `convert_to_llm(agent_messages, provider) -> ProviderMessage[]`
- `interpret_llm_output(provider_output) -> AgentAction[]`

This keeps the loop portable across providers.

## Tooling Model

The first tool-capable version of the loop should support:

- one tool request at a time
- structured args
- structured result
- deterministic failure/result insertion
- checkpoint before and after execution

Later we can add:

- multiple tool calls in one step
- tool batching
- long-running tool futures

## Approval Model

Approvals should be explicit interrupts, not prompt hacks.

Recommended behavior:

- model emits `request_approval`
- turn moves to `waiting_approval`
- session persists approval payload and required actor
- slash commands or transport UX resolve approval
- same `turn_id` resumes with approval resolution appended

This is the right place to copy LangGraph in spirit.

## Slash Commands

Slash commands are part of the runtime contract, but not all are loop work.

Rules:

- deterministic commands bypass the loop
- agent-assisted commands enter the loop as structured tasks
- all commands still belong to a `session_id` / `turn_id`

Current implemented example:

- `/ping`

## Recommended Implementation Sequence

### Project 1: Loop Spec and Contracts

- define turn states
- define action schema
- define checkpoint boundaries
- define event stream schema

### Project 2: Single-Step Structured Response

- replace plain text model completion with action-oriented completion
- support `respond` and `fail`

### Project 3: Single Tool Loop

- support `tool_call -> tool_result -> respond`
- checkpoint before and after tool execution

### Project 4: Approval Interrupts

- support `request_approval`
- persist interrupt state
- resume same turn

### Project 5: Follow-up / Steering

- add Pi-style follow-up and steering hooks
- preserve event and checkpoint semantics

## Full Recommendation

Philotic should not reinvent the loop from scratch, and it should not directly embed provider SDK orchestration as the source of truth.

It should:

- use **Pi as the core turn-engine template**
- use **Philotic sessions as the durable execution substrate**
- use **structured actions and results**
- use **checkpointed super-steps**
- use **real approval interrupts**

This is the point where the agent project becomes its own system rather than "some extra logic in `philote`."
