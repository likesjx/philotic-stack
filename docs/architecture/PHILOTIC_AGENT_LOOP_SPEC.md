# Philotic Agent Loop Spec

## Scope

This document specifies the implementation contract for the Philotic agent loop.

It covers:

- durable turn states
- internal action model
- checkpoint boundaries
- provider boundary
- tool execution contract
- approval interrupt contract
- event stream contract

It does **not** specify transport-specific UX.

## Canonical Ownership

- `ansible`
  - canonical session and turn state
  - event timeline
  - recovery snapshot composition
- `agent-core`
  - in-flight working loop state
  - provider interaction
  - tool orchestration
  - checkpoint projection generation

Apartment checkpoints remain derived recovery projections, not canonical session truth.

## Identifiers

- `session_id`
  - durable branch/workflow identity
- `turn_id`
  - one round-trip execution identity within a session
- `step_id`
  - optional per-super-step identifier within a turn
- `action_id`
  - optional identifier for tool or approval actions

## Durable Turn States

Canonical turn states:

- `queued`
- `loading_context`
- `thinking`
- `waiting_tool`
- `waiting_approval`
- `resuming`
- `completed`
- `failed`

### State transitions

- `queued -> loading_context`
- `loading_context -> thinking`
- `thinking -> waiting_tool`
- `thinking -> waiting_approval`
- `thinking -> completed`
- `thinking -> failed`
- `waiting_tool -> thinking`
- `waiting_approval -> resuming`
- `resuming -> thinking`
- `resuming -> completed`
- `resuming -> failed`

No terminal state may transition back to a non-terminal state.

## Internal Action Model

Each super-step yields one or more structured actions.

### Action kinds

- `respond`
- `tool_call`
- `request_approval`
- `handoff`
- `fail`

### Action payloads

#### `respond`

```json
{
  "kind": "respond",
  "content": "Final answer text"
}
```

#### `tool_call`

```json
{
  "kind": "tool_call",
  "tool_name": "workspace.read_file",
  "arguments": {
    "path": "/tmp/demo.txt"
  }
}
```

#### `request_approval`

```json
{
  "kind": "request_approval",
  "approval_type": "tool_execution",
  "reason": "Need approval before shell execution",
  "requested_action": {
    "tool_name": "shell.exec",
    "arguments": {
      "cmd": "rm -rf /tmp/demo"
    }
  }
}
```

#### `handoff`

```json
{
  "kind": "handoff",
  "target_component": "planner",
  "payload": {
    "objective": "Break task into milestones"
  }
}
```

#### `fail`

```json
{
  "kind": "fail",
  "error_code": "MAX_ITERATIONS",
  "message": "Turn exceeded iteration limit"
}
```

## Internal Message Model

Philotic should keep a Pi-style neutral message layer.

Recommended internal message kinds:

- `system`
- `user`
- `assistant`
- `tool_request`
- `tool_result`
- `approval_request`
- `approval_result`
- `steering`

These are the messages transformed by `transformContext(...)` before provider conversion.

## Provider Boundary

Required hooks:

### `transform_context`

Input:

- canonical session snapshot
- local working turn state
- optional steering/follow-up messages

Output:

- normalized internal agent messages

### `convert_to_llm`

Input:

- internal agent messages
- provider/model controller

Output:

- provider-native request payload

### `interpret_llm_output`

Input:

- provider-native response

Output:

- structured Philotic actions
- optional assistant message fragments
- optional provider continuation metadata

Provider-native reasoning metadata may be preserved in checkpoints/events, but it must not become the canonical session representation.

## Checkpoint Boundaries

Philotic checkpoints after every super-step.

Required checkpoint points:

### 1. After context load

- turn state: `loading_context`
- checkpoint includes:
  - active turn
  - recent turns
  - effective bindings

### 2. After model output interpretation

- turn state: `thinking`
- checkpoint includes:
  - interpreted actions
  - assistant draft/summary if present

### 3. Before tool execution

- turn state: `waiting_tool`
- checkpoint includes:
  - pending tool request

### 4. After tool execution

- turn state: `thinking`
- checkpoint includes:
  - tool result

### 5. Before approval pause

- turn state: `waiting_approval`
- checkpoint includes:
  - approval request payload

### 6. On resume

- turn state: `resuming`
- checkpoint includes:
  - approval resolution

### 7. On finalization

- turn state: `completed` or `failed`
- checkpoint includes:
  - final response or failure payload

## Tool Execution Contract

### MVP tool rules

- one tool call at a time
- deterministic request/response envelope
- structured success/failure result
- checkpoint before and after execution

### Tool request envelope

```json
{
  "action_id": "tool-123",
  "tool_name": "workspace.read_file",
  "arguments": {
    "path": "/tmp/demo.txt"
  }
}
```

### Tool result envelope

```json
{
  "action_id": "tool-123",
  "tool_name": "workspace.read_file",
  "ok": true,
  "result": {
    "content": "hello"
  }
}
```

### Tool failure envelope

```json
{
  "action_id": "tool-123",
  "tool_name": "workspace.read_file",
  "ok": false,
  "error_code": "NOT_FOUND",
  "message": "File does not exist"
}
```

## Approval Interrupt Contract

Approvals are persisted interrupts.

### Pause contract

When an approval is required:

- append approval request event
- set turn state to `waiting_approval`
- persist approval payload in session/turn state
- stop loop execution

### Resume contract

When approval resolves:

- append approval result event
- reload session snapshot
- re-enter loop with same `turn_id`
- set turn state to `resuming`

### Approval resolution payload

```json
{
  "approval_id": "approval-123",
  "decision": "approved",
  "actor_id": "user-telegram-123",
  "note": "Proceed"
}
```

## Event Stream Contract

Philotic should emit structured loop events.

Recommended event kinds:

- `turn_started`
- `context_loaded`
- `model_step_started`
- `model_step_completed`
- `tool_requested`
- `tool_started`
- `tool_completed`
- `tool_failed`
- `approval_requested`
- `approval_resolved`
- `turn_completed`
- `turn_failed`

These should align with both:

- session event persistence
- future streaming/observability hooks

## Iteration Boundaries

Recommended initial loop limits:

- `max_iterations_per_turn = 8`
- `max_tool_calls_per_turn = 8`
- `max_approval_interrupts_per_turn = 2`

Exceeding bounds should emit a `fail` action with structured error code.

## Slash Command Handling

Before loop entry:

- parse deterministic slash commands
- if command is local and deterministic:
  - do not invoke provider
  - still create/update session and turn records
  - finalize immediately

Current implemented command:

- `/ping -> pong`

## Session Snapshot Requirements For Loop Entry

The loop expects a session snapshot containing at least:

- `session_id`
- `agent_id`
- `status`
- `recent_turns`
- `active_turn`
- `summary`
- `session_index`

Future required fields:

- `effective_toolset`
- `effective_skillset`
- `effective_model_controller`
- `effective_workspace_ref`
- `effective_policy`

## Implementation Phases

### Phase 1

- structured `respond` / `fail`
- checkpointed single-step completion

### Phase 2

- single `tool_call`
- tool result insertion
- bounded loop continuation

### Phase 3

- approval interrupts
- resume same `turn_id`

### Phase 4

- handoffs
- steering/follow-up hooks
- richer event streaming

## Non-Goals

For the first implementation:

- multiple concurrent tool calls in one turn
- provider-specific deep reasoning persistence as canonical truth
- cross-session planning trees
- speculative branching within one live turn

## Definition Of Done

The first real Philotic loop is complete when:

- a turn can produce `respond` or `tool_call`
- tool results re-enter the loop correctly
- turn state is checkpointed between super-steps
- approval-required actions pause and resume durably
- crash recovery can rebuild an in-flight turn from the canonical session snapshot plus checkpoint projection
