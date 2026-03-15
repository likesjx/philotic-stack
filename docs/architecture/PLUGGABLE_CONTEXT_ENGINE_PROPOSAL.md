---
title: "Pluggable Context Engine Proposal"
doc_type: proposal
domain: memory-context
status: accepted-current-slice
last_updated: 2026-03-12
tags:
  - context
  - engine
  - assembly
  - memory
  - active-seam
related_docs:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
  - MEMORY_ENGINE_ABSTRACTION_PROPOSAL.md
  - AGENT_PLUGIN_HOOKS_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: pluggable-context-engine
implements: []
implemented_by: []
active_seams:
  - context-engine-contract
  - deterministic-context-assembly
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
---

# Pluggable Context Engine Proposal

## Goal

Define a clean boundary for how Philotic assembles context for a `conversation turn` so context sources, retrieval strategies, and ranking policies can evolve without turning `philote` into one giant opinionated glue pile.

## Disposition

`accepted for current slice`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Current Slice

Prove the first typed context projection path without pretending the full context engine has arrived:

- coin `conversation turn` as the externally meaningful exchange boundary
- coin `cognitive step` as the internal reasoning/action boundary within a conversation turn
- define the first five context layers and their ownership model
- introduce typed `ContextProjection` / `LayerContribution` / `LayerPayload` structures in `philote`
- thread the first structured context path into outbound model requests and through `model-router`
- keep the broader context-engine contract transitional while the full provider/hook architecture is still pending

Current confidence for this slice:

- `test-green`
  - `cargo test -p philote -- --nocapture`
  - `cargo test -p model-router -- --nocapture`
- `smoke-green` for the current cognitive request path
  - `bash scripts/smoke-cognitive-roundtrip.sh`
- not yet `watched-live-green`

## Core Recommendation

Introduce a **context engine** contract that is owned by the hotel/runtime boundary, not by one model provider or one agent implementation.

That engine should:

- collect candidate context from canonical sources
- rank, filter, and budget that context deterministically
- expose a stable turn-ready context payload to `philote`
- allow multiple implementations behind one contract

The engine should reason in two scopes:

- `conversation turn`
  - the externally meaningful exchange boundary, such as an inbound user message, slash command, approval reply, or delegated return
- `cognitive step`
  - one internal reasoning/action step inside a conversation turn, such as context build, model call, tool call, tool-result integration, or response finalization

These terms can carry short aliases in discussion:

- `exchange` = `conversation turn`
- `thought step` = `cognitive step`

But architecture docs and code-facing contracts should use the canonical pair:

- `conversation turn`
- `cognitive step`

## Why This Needs To Exist

Philotic is accumulating multiple context sources:

- session snapshot
- imported agent identity bundles
- graph-backed profile/config
- memory systems
- tool and capability state
- future external retrieval engines

If `philote` owns all of that assembly directly, it becomes impossible to change context strategy without changing the cognitive loop itself.

It also becomes too easy to let one ambiguous word, `turn`, quietly mean both:

- the user-visible exchange boundary
- the internal model/tool loop iteration

Philotic should stop paying that ambiguity tax now instead of after the APIs start pretending they meant the same thing all along.

## Recommended Boundary

The context engine should own:

- source selection
- ordering and ranking
- token/size budgeting
- deterministic inclusion/exclusion rules
- provenance metadata for debugging

`philote` should consume:

- a bounded, ordered context payload
- not a pile of half-ranked raw records

The context engine should not become the canonical owner of all context-bearing state.

Instead, it should compose from the current owners of truth:

- Philotic context graph for durable identity, session, and structural relationship state
- memory engines such as Muninn for recalled salience, episodic continuity, and learned preferences
- `philote` for ephemeral working state inside the active conversation turn

## First Context Layers

The first contract should explicitly model five layers.

| Layer | Canonical owner | Authority | Mutability | Refresh timing | Promotion target |
| --- | --- | --- | --- | --- | --- |
| `identity` | Philotic context graph | authoritative | `static_for_turn` | conversation-turn start unless explicitly reconfigured | graph-backed profile/config only |
| `relationship` | graph + memory engine | mixed | `refreshable` | conversation-turn start, then when the relationship frame changes materially | memory first, graph only for durable stable facts |
| `session` | Philotic context graph | authoritative for current runtime truth | `refreshable` | conversation-turn start and named checkpoints | graph-backed session/event state |
| `working` | `philote` | authoritative only inside the active conversation turn | `live_local` | every relevant cognitive step | checkpoint summary only, never raw scratch |
| `knowledge` | graph + memory engine | mixed, mostly advisory | `refreshable` | conversation-turn start plus retrieval checkpoints after meaningfully new information | memory summary, graph links/facts when stable enough |

### 1. Identity Layer

- canonical owner: Philotic context graph
- purpose: self, role, posture, soul, identity
- authority: authoritative
- mutability: usually `static_for_turn`

### 2. Relationship Layer

- canonical owner: split
- graph owns stable principal/user structure and durable relationship facts
- memory engine owns recalled collaboration memory and softer learned fit
- authority: mixed
- mutability: usually `refreshable`

### 3. Session Layer

- canonical owner: Philotic context graph
- purpose: participants, approvals, bindings, active incarnation, recent turn transcript, transport context
- authority: authoritative for current runtime truth
- mutability: `refreshable`

### 4. Working Layer

- canonical owner: `philote`
- purpose: active scratch state, tool history, local hypotheses, pending subgoals
- authority: authoritative only inside the current conversation turn
- mutability: `live_local`

### 5. Knowledge Layer

- canonical owner: mixed
- graph contributes structural truth and traversable relationships
- memory engine contributes episodic salience and recalled summaries
- authority: mostly advisory, unless a graph-backed fact is explicitly authoritative
- mutability: `refreshable`

## Mutability Classes

The first context-engine contract should classify layer refresh policy explicitly:

- `static_for_turn`
  - loaded once at conversation-turn start
- `refreshable`
  - recomputed at named checkpoints within the turn
- `live_local`
  - changes continuously inside `philote` during cognitive steps

This prevents the usual architectural comedy where everything is declared dynamic and nothing is actually explainable.

## Provider Contract

The engine should gather context through explicit layer providers rather than one monolithic builder.

Recommended first shape:

- `collect(layer_id, turn_context) -> LayerContribution[]`

Each `LayerContribution` should carry:

- `source_id`
- `layer_id`
- `content`
- `authority`
- `confidence`
- `freshness`
- `provenance`
- `budget_cost`
- `advisory_or_authoritative`

Recommended first provider families:

- graph identity provider
- graph relationship provider
- session snapshot provider
- working-state provider
- memory provider
- graph knowledge provider
- legacy import provider

## First Contract Shapes

The first implementation does not need a full Rust trait hierarchy on day one, but it should converge on a clear payload vocabulary.

### `ConversationTurnScope`

Use this shape for the external exchange boundary:

- `conversation_turn_id`
- `session_id`
- `agent_id`
- `source`
- `primary_user_id`
- `trigger_kind`
  - examples: `user_message`, `slash_command`, `approval_resolution`, `delegated_return`
- `started_at`

### `CognitiveStepScope`

Use this shape for one internal reasoning/action step inside a conversation turn:

- `conversation_turn_id`
- `cognitive_step_id`
- `step_kind`
  - examples: `context_build`, `model_call`, `tool_call`, `tool_result`, `response_finalize`
- `iteration`
- `checkpoint`
- `started_at`

### `LayerContribution`

Each provider contribution should be normalized into one record shape:

- `contribution_id`
- `layer_id`
- `source_id`
- `content`
- `summary`
- `authority`
  - `authoritative`
  - `advisory`
- `confidence`
- `freshness`
- `budget_cost`
- `provenance`
- `expires_at`
  - optional; mainly for refreshable contributions

### `ContextProjection`

The context engine should emit one bounded projection per conversation turn:

- `conversation_turn`
  - `ConversationTurnScope`
- `active_step`
  - optional `CognitiveStepScope`
- `layers`
  - ordered layer payloads ready for model/runtime consumption
- `contributions`
  - normalized selected `LayerContribution[]`
- `budget`
  - token/size accounting and trim decisions
- `refresh_plan`
  - which layers should be reconsidered at which checkpoints
- `provenance_trace`
  - debug path showing why items were included or dropped

### `LayerPayload`

Each emitted layer in `ContextProjection.layers` should contain:

- `layer_id`
- `owner`
- `authority`
- `rendered_content`
- `source_refs`
- `refreshable`
- `promotion_hint`
  - `none`
  - `memory_candidate`
  - `graph_candidate`
  - `checkpoint_only`

## First Proof Path

The first honest implementation path should prove one real `conversation turn` assembly path before broadening the engine story.

Recommended first path:

- `identity` + `session`
  - sourced from the current Philotic session snapshot and graph-backed agent profile
- `relationship` + `knowledge`
  - sourced from Muninn recall using the current triad prompts
- `working`
  - remains local to `philote` and is surfaced only through checkpoint summaries

That split proves the boundary without forcing Philotic to solve all long-term memory metaphysics before it can assemble one decent turn.

## Projection And Budgeting

The context engine should emit a bounded `ContextProjection` for one conversation turn.

That projection should include:

- ordered layer outputs
- provenance for debugging
- budget accounting
- refresh hints for later cognitive-step checkpoints
- authority markers so advisory memory does not quietly impersonate canonical state

## Refresh Model

Context should be assembled per `conversation turn`, then selectively refreshed at `cognitive step` checkpoints.

Recommended checkpoints:

1. `conversation_turn.start`
2. `cognitive_step.context_build`
3. `checkpoint.before_model`
4. `checkpoint.after_model`
5. `checkpoint.after_tool`
6. `checkpoint.before_reply`
7. `conversation_turn.end`

## First Implementations To Support

1. graph-native context assembly
2. imported OpenClaw/ZeroClaw identity projection
3. memory-backed augmentation
4. emergency/local fallback context mode

## First Slice Recommendation

Define the first trait or request/response contract for:

- `build_context(agent_id, session_id, conversation_turn_id, mode)`

Then make the current inline assembly path one implementation of that contract, with:

- named `conversation turn` and `cognitive step` terminology
- the five initial layers above
- explicit mutability classes
- a compact layer contract table for owner, authority, refresh, and promotion behavior
- deterministic provenance and budget metadata

That gives Philotic a real skeleton for growth instead of a growing pile of context ingredients waiting for a chef to confess which kitchen owns the stove.
