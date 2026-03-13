---
title: "Agent Plugin Hooks Proposal"
doc_type: proposal
domain: tooling-execution
status: accepted-current-slice
last_updated: 2026-03-12
tags:
  - hooks
  - plugins
  - agent-core
  - extensibility
  - active-seam
related_docs:
  - ARCHITECTURE_STATUS.md
  - PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md
  - MEMORY_ENGINE_ABSTRACTION_PROPOSAL.md
  - TASK_RUNNER_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: agent-plugin-hooks
implements: []
implemented_by: []
active_seams:
  - agent-hook-registry
  - transcription-hook-extraction
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
---

# Agent Plugin Hooks Proposal

## Goal

Define the plugin/hook boundaries `agent-core` should expose so new context engines, memory engines, local models, and control-plane behaviors can be integrated without repeatedly cutting into the main turn loop.

## Disposition

`accepted for current slice`

Track related work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Current Slice

Define the first hook vocabulary and payload shapes so hook integration can grow around a stable context skeleton:

- align hook timing to `conversation turn` and `cognitive step`
- define explicit checkpoints instead of generic callback soup
- separate context collection, refresh, promotion, and post-processing responsibilities
- land the first typed `HookRequest` / `HookResult` / `RefreshRequest` / `PromotionAction` payloads in `agent-core`
- keep the first hook registry itself for a later seam

## Core Recommendation

Treat `agent-core` as a host for bounded extension hooks, not as the permanent owner of every new runtime concern.

The first hook families should cover:

- context assembly
- memory lookup/store
- media transforms such as transcription
- model capability routing hints
- admin/control intercepts

Hook contracts should use the same canonical vocabulary as the context engine:

- `conversation turn`
  - the external exchange boundary
- `cognitive step`
  - one internal reasoning/action step within that turn

Short aliases are fine in discussion:

- `exchange`
- `thought step`

But the contracts themselves should stay canonical to avoid rediscovering ambiguity under a cooler name.

## Why This Matters

Philotic is already plugin-shaped at the system level, but `agent-core` still absorbs too much behavior directly.

Without hooks:

- every new subsystem becomes a loop edit
- testing gets harder
- “plugin architecture” stays true everywhere except the place that hurts most

Without explicit timing, hooks also become hard to reason about:

- some data should be gathered once per conversation turn
- some should refresh after specific cognitive steps
- some should remain purely local to the active step

If those scopes are left implicit, plugins will either miss needed updates or mutate state at the wrong level of truth.

## Recommended Hook Style

Prefer explicit contracts over magical callbacks.

Examples:

- `context.build`
- `memory.search`
- `memory.store`
- `media.transcribe`
- `response.postprocess`
- `admin.intercept`

These can be implemented by local components, hotel-mediated tools, or future plugin runners.

## Hook Timing Model

Hooks should run at named lifecycle points.

Recommended first sequence:

1. `conversation_turn.start`
2. `cognitive_step.context_build`
3. `checkpoint.before_model`
4. `checkpoint.after_model`
5. `checkpoint.after_tool`
6. `checkpoint.before_reply`
7. `conversation_turn.end`

This lets Philotic distinguish:

- what is stable for the whole conversation turn
- what can be refreshed at a later cognitive step
- what should remain private local working state

## First Hook Families

The first bounded hook families should be:

- `context.build`
  - collect and rank layer contributions for a conversation turn
- `relationship.refresh`
  - refresh user/operator memory when the turn meaningfully changes shape
- `knowledge.refresh`
  - re-run topic retrieval after tool results or goal shifts
- `working.capture`
  - capture local working-state updates after model/tool steps
- `memory.promote`
  - decide what, if anything, becomes durable after the conversation turn ends
- `response.postprocess`
  - shape delivery artifacts without mutating canonical state
- `admin.intercept`
  - enforce control-plane or policy boundaries before dangerous actions

## Hook Contract Style

Prefer explicit request/response contracts such as:

- `run_hook(hook_name, scope, checkpoint, payload) -> HookResult`

Where:

- `scope` distinguishes `conversation_turn` from `cognitive_step`
- `checkpoint` names the lifecycle moment
- `payload` carries only the bounded context needed for that hook family

The system should avoid magical mutation by side effect. If a hook wants to change durable truth, it should emit an explicit promotion or update request rather than smuggling ontology changes through a callback.

## First Hook Payload Shapes

The first hook registry should converge on a small shared payload vocabulary.

### `HookRequest`

- `hook_name`
- `scope`
  - `conversation_turn`
  - `cognitive_step`
- `checkpoint`
- `conversation_turn`
  - conversation-turn scope payload
- `cognitive_step`
  - optional cognitive-step scope payload
- `context_projection`
  - optional current projection snapshot
- `inputs`
  - hook-family-specific arguments

### `HookResult`

- `status`
  - `applied`
  - `skipped`
  - `deferred`
  - `failed`
- `updates`
  - bounded payload updates for the caller
- `emitted_contributions`
  - optional `LayerContribution[]`
- `refresh_requests`
  - optional `RefreshRequest[]`
- `promotion_actions`
  - optional `PromotionAction[]`
- `diagnostics`
  - provenance/debug notes

### `RefreshRequest`

Use this when a hook wants the context engine to reconsider one or more layers.

- `layer_ids`
- `reason`
- `target_checkpoint`
- `urgency`
  - `immediate`
  - `next_checkpoint`
  - `turn_end`

### `PromotionAction`

Use this when a hook wants to propose durable write-back rather than mutating canonical state directly.

- `target`
  - `memory`
  - `graph`
  - `session_checkpoint`
- `concept`
- `summary`
- `content`
- `confidence`
- `reason`
- `source_refs`

## First Proof Hooks

The first live seams moved behind hooks should stay narrow.

Recommended proof order:

1. `context.build`
   - wraps the current session snapshot plus memory recall path
2. `knowledge.refresh`
   - proves checkpoint-driven refresh after tool results
3. `memory.promote`
   - proves turn-end durable write-back without letting hooks become secret ontology writers

That sequence exercises the timing model without turning the hook registry into a ceremony machine before it has done any useful work.

## First Slice Recommendation

Define the first hook registry and contract shapes, then move one current seam behind it:

- transcription
- or memory lookup

That proves the extension model before it turns into an abstraction festival.
