---
title: Scripted Turn Loop Variants Proposal
doc_type: proposal
domain: runtime-sessions
status: draft
last_updated: 2026-03-31
tags:
- agent-loop
- skills
- planning
- approval
- governance
- runtime
related_docs:
- PHILOTIC_AGENT_LOOP_SPEC.md
- GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md
- SKILL_LIFECYCLE_PROPOSAL.md
- APPROVAL_UX_PROPOSAL.md
- ROLE_ACTIVATION_AND_SUBAGENT_CONTRACTS_PROPOSAL.md
proposal_id: scripted-turn-loop-variants
---

# Scripted Turn Loop Variants Proposal

## Goal

Replace the hard-coded single agent turn loop with a scriptable tree of named steps that can be loaded per-role via the skill system. New loop behaviors — plan-then-approve-then-execute, reflect-then-respond, multi-stage reasoning — become skill definitions, not Rust code changes.

The `plan_execute` variant is the motivating case: the agent plans its full tool sequence, the operator approves the entire plan in one interaction, and then execution proceeds without further interruption — as long as execution matches the approved plan.

---

## Problem

The current turn loop in `philote` is a hard-coded single path:

```
receive task
  → load context
  → model_call
  → [tool_call → model_call]* (per-tool loop)
  → respond
```

Two concrete problems:

1. **Per-tool approval friction** — the operator must approve each tool call individually. For multi-step plans, this is unworkable.
2. **No loop extensibility** — adding a new loop shape (e.g. reflect-then-respond, staged reasoning) requires a Rust code change and a deploy.

---

## Core Design

### LoopScript

A `LoopScript` is a JSON-serializable ordered list of `LoopStep` nodes. It lives in `TurnLoopConfig` alongside the existing scalar fields (`iteration_cap`, `approval_policy`, etc.).

`TurnLoopConfig` gains one new optional field:

```json
{
  "iteration_cap": 10,
  "loop_script": {
    "variant": "plan_execute",
    "steps": [ ... ]
  }
}
```

When `loop_script` is absent, philote runs the standard loop unchanged. When it is present, philote dispatches to `ScriptedLoopExecutor`.

### LoopStep

Each step has an `id`, a `type`, and optional `input` (a binding to a prior step's output by step id) and `config` (type-specific parameters):

```json
{
  "id": "plan",
  "type": "model_call",
  "config": {
    "phase": "plan",
    "output_schema": "plan_proposal"
  }
}
```

Steps are executed in declaration order. The executor maintains a `StepContext` map from step id → output value for downstream bindings.

### Step Input Bindings

`input` is a dot-path into a prior step's output:

```json
{ "input": "plan.steps" }
```

The executor resolves this before dispatching the step. A missing binding is a turn failure (not a silent skip).

---

## Step Vocabulary

### `model_call`

Invoke the model. The `phase` label is injected into the system prompt to orient the model's output shape.

| Config field | Type | Description |
|---|---|---|
| `phase` | string | Prompt phase label: `plan`, `synthesize`, `reflect`, `respond` |
| `output_schema` | string? | Expected output schema name. If set, the executor validates shape. |

Output: the model's structured response (parsed from JSON if schema set, raw text otherwise).

### `approval_gate`

Pause the turn. Emit the bound input as a structured approval request to the operator. Resume when operator replies `approved` or `rejected`.

| Config field | Type | Description |
|---|---|---|
| `gate` | string | Gate kind: `operator` (default), `admin` |
| `surface_as` | string? | Presentation hint for the membrane: `plan_card`, `diff_card`, `raw` |
| `reject_action` | string | On rejection: `fail_turn` (default) or `rollback_to:<step_id>` |

On approval: resume at the next step with the approval token in context.
On rejection: `fail_turn` emits a `respond` with the rejection reason; `rollback_to` re-runs from the named step.

This is implemented via the existing `waiting_approval → resuming` turn state transition from `PHILOTIC_AGENT_LOOP_SPEC.md`. No new turn states required.

### `tool_sequence`

Execute an ordered list of tool calls sourced from a prior step's plan output.

| Config field | Type | Description |
|---|---|---|
| `abort_on_mismatch` | bool | If true, fail the turn if the model attempts to call a tool not in the approved plan |
| `emit_partial_results` | bool | If true, stream each tool result to the membrane before synthesis |

Input must resolve to a `plan_proposal.steps` array (see Plan Proposal Schema below).

Output: array of `{ tool_name, arguments, result, ok }` objects.

### `tool_call`

Single tool call. For simple loops that need one predetermined tool without a full plan.

| Config field | Type | Description |
|---|---|---|
| `tool_name` | string | The tool to call |
| `arguments` | object? | Static arguments. Dynamic arguments can be bound from `input`. |

### `checkpoint`

Force a `SyncApartment` checkpoint mid-loop. Useful for long-running loops that should persist state before a risky next step.

No config fields.

### `branch`

Conditional routing. Evaluates a condition against a prior step's output and routes to one of two named continuation steps.

| Config field | Type | Description |
|---|---|---|
| `condition` | string | Dot-path predicate, e.g. `plan.requires_shell == true` |
| `if_true` | string | Step id to jump to on true |
| `if_false` | string | Step id to jump to on false |

### `emit_event`

Fire a mesh event (for agent-graph integration and observability).

| Config field | Type | Description |
|---|---|---|
| `event_kind` | string | e.g. `loop.plan_approved`, `loop.execution_complete` |
| `payload_from` | string? | Dot-path binding to include in event payload |

---

## Plan Proposal Schema

When `output_schema: "plan_proposal"` is set on a `model_call` step, the executor validates the model's output against this shape:

```json
{
  "objective": "One-sentence summary of what this plan accomplishes",
  "steps": [
    {
      "step_index": 0,
      "tool_name": "workspace.read_file",
      "arguments": { "path": "/tmp/foo.txt" },
      "rationale": "Need to read the file before editing it"
    }
  ],
  "risks": "Optional: what could go wrong, what requires care",
  "estimated_steps": 2
}
```

The `steps` array is what `tool_sequence` consumes via `input: "plan.steps"`.

Validation failures (missing required fields, steps not an array, etc.) cause the step to fail with `output_schema_mismatch` and the turn fails cleanly without proceeding to approval.

---

## The `plan_execute` Variant

This is the first built-in named variant. It is the motivating use case.

```json
{
  "variant": "plan_execute",
  "steps": [
    {
      "id": "plan",
      "type": "model_call",
      "config": {
        "phase": "plan",
        "output_schema": "plan_proposal"
      }
    },
    {
      "id": "approve",
      "type": "approval_gate",
      "input": "plan",
      "config": {
        "gate": "operator",
        "surface_as": "plan_card",
        "reject_action": "fail_turn"
      }
    },
    {
      "id": "execute",
      "type": "tool_sequence",
      "input": "plan.steps",
      "config": {
        "abort_on_mismatch": true,
        "emit_partial_results": true
      }
    },
    {
      "id": "synthesize",
      "type": "model_call",
      "input": "execute",
      "config": {
        "phase": "synthesize"
      }
    }
  ]
}
```

Operator experience:
1. User sends a request.
2. Agent produces a plan (tool list with rationale).
3. Membrane renders it as a plan card: "Here's what I'm going to do: [step list]. Approve?"
4. Operator sends `/approve` (or taps the button).
5. Tools execute in sequence without further interruption.
6. Agent synthesizes and responds.

---

## Loading Loop Scripts via Skills

A skill whose `skill_type` is `loop_variant` carries a `loop_script` payload instead of (or in addition to) a prompt fragment. When the skill is activated on a session or role, the loop script is written into `turn_loop_config.loop_script`.

Skill activation:

```
/skills add plan-execute
```

This triggers `skill.register` → hotel persists skill record with `loop_script` → next `compose_session_snapshot` injects the updated `turn_loop_config` → philote picks up the new script on the next turn.

Skill deactivation:

```
/skills remove plan-execute
```

Removes the `loop_script` field, reverting to the standard loop.

### Skill Record Extension

`AbstractSkillRecord` gains an optional `loop_script` field:

```json
{
  "skill_id": "plan-execute",
  "skill_type": "loop_variant",
  "display_name": "Plan + Approve + Execute",
  "description": "Agent plans its full tool sequence, operator approves the entire plan once, then execution proceeds.",
  "loop_script": { ... }
}
```

`skill_type: "loop_variant"` skills are validated against the `LoopScript` schema at `draft → validated` time (Layer 1 of the skill lifecycle). No runtime surprise.

---

## Enforcement: abort_on_mismatch

When `abort_on_mismatch: true` is set on a `tool_sequence` step and the model attempts to call a tool during synthesis that is not in the approved plan, the executor:

1. Intercepts the tool call before dispatch.
2. Emits a `loop.mismatch` event to the hotel.
3. Fails the turn with `action_kind: "fail"`, `error_code: "PLAN_MISMATCH"`.
4. Surfaces a message to the operator: "Execution deviated from the approved plan. Turn aborted."

This is the enforcement contract: approval covers exactly the plan presented, not the model's subsequent whims.

---

## Re-entry Mechanism

`approval_gate` re-entry follows the same pattern as the existing `voice.transcribe` re-entry:

1. Turn parks at `waiting_approval`.
2. Hotel stores the current `StepContext` in the session apartment under `__loop_reentry__`.
3. When the operator approves, the hotel emits a `ResumeTask` with the approval token.
4. philote's re-entry path rehydrates `StepContext` from the apartment and continues at the next step.

No new turn state machine entries required — `waiting_approval → resuming` already covers this.

---

## Relationship to Existing Specs

| Spec | Relationship |
|---|---|
| `PHILOTIC_AGENT_LOOP_SPEC.md` | This proposal is additive. Standard loop is unchanged. Turn states are unchanged. |
| `GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md` | Loop variant skills are a new `skill_type` within the governed workflow layer. |
| `SKILL_LIFECYCLE_PROPOSAL.md` | Layer 1 validation extends to `LoopScript` schema for `loop_variant` skills. |
| `APPROVAL_UX_PROPOSAL.md` | `approval_gate` with `surface_as: "plan_card"` is a new card type for the membrane approval UX. |

---

## What Needs to Be Built

### Phase 1 — Core infrastructure

1. **`LoopScript` / `LoopStep` types** in `ansible-mesh-core::graph` — serializable, validated
2. **`TurnLoopConfig.loop_script`** field (optional) — zero-default, backward compatible
3. **`ScriptedLoopExecutor`** in `philote` — walks steps, binds outputs, handles re-entry
4. **`plan_proposal` schema validator** — lightweight: required fields + array shape check
5. **`approval_gate` re-entry** — `StepContext` park/rehydrate via session apartment

### Phase 2 — Skill integration

6. **`loop_variant` skill type** in skill record schema
7. **`skill.register` handler extension** — validate `loop_script` at Layer 1
8. **`compose_session_snapshot` extension** — inject `loop_script` from active skill into `turn_loop_config`
9. **`plan_card` membrane surface** — render plan proposals as structured approval cards in Telegram

### Phase 3 — Enforcement and observability

10. **`abort_on_mismatch` interceptor** — validate live tool calls against approved plan
11. **`loop.*` mesh events** — `plan_proposed`, `plan_approved`, `plan_rejected`, `execution_complete`, `mismatch`
12. **Built-in `plan-execute` skill definition** in `mesh-config.example.json`

---

## Out of Scope

- Parallel step execution (all steps are sequential in this proposal)
- Cross-turn plan persistence (plan is scoped to the current turn)
- Dynamic step injection by the model during execution (the script is fixed at turn start)
- Loop script hot-swap mid-turn (changes take effect on the next turn)
