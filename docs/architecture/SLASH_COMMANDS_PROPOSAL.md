# Philotic Slash Commands Proposal

## Goal

Add a first-class slash-command path that can short-circuit the normal agent loop when the user is clearly addressing the system rather than asking for open-ended reasoning.

This gives us a fast, deterministic control surface without forcing every input through `agent-core -> model-router -> agent-core`.

## Why This Matters

Some requests are a terrible fit for the full cognition loop:

- operational controls
- deterministic status queries
- approval actions
- session lifecycle actions
- debugging and smoke-test commands

Using the agent for those anyway is the kind of elegant overengineering that feels powerful right up until it becomes latency with extra steps.

## Core Recommendation

Treat slash commands as structured events, not just text prompts with a funny hat.

The path should be:

- transport receives raw user input
- if input begins with `/`, parse it before normal turn reasoning
- route the command to the appropriate executor
- only invoke the LLM if the command class explicitly requires agent assistance

## Command Classes

### 1. Platform Commands

These bypass the model entirely.

Examples:

- `/help`
- `/status`
- `/session`
- `/pause`
- `/resume`
- `/fork`
- `/checkpoint`
- `/approve`
- `/deny`

These should execute in `membrane`, `ansible`, or another runtime service depending on ownership.

### 2. Agent Commands

These are addressed to the agent, but still deterministic and local.

Examples:

- `/ping`
- `/memory`
- `/tools`
- `/skills`
- `/workspace`

These should execute inside `agent-core` and may update checkpoints or session metadata, but they should not invoke `model-router`.

### 3. Agent-Assisted Commands

These are commands syntactically, but still need the LLM.

Examples:

- `/plan <goal>`
- `/summarize <artifact>`
- `/search <query>`

These should enter `agent-core` with structured intent and arguments, then proceed through the normal reasoning path.

## Ownership Model

Recommended execution ownership:

- `membrane`
  - transport-local UX commands
  - transport help text
  - future command autocomplete/help menus
- `ansible`
  - generalized session/runtime commands
  - session lifecycle transitions
  - lease/materialization/debug control
- `agent-core`
  - agent-local deterministic commands
  - commands that inspect current cognitive/session state
- `model-router`
  - no direct slash-command ownership
  - only participates when an agent-assisted command invokes the model

## Payload Shape

Keep the communication plane general by encoding parsed commands as structured task payloads.

Example:

```json
{
  "kind": "command",
  "command": "fork",
  "args": ["turn-7"],
  "session_id": "telegram:123:agent-jane-01",
  "turn_id": "telegram-update-1003",
  "source": "telegram",
  "chat_id": "123"
}
```

For MVP we can infer slash commands directly from `content`, but the longer-term recommendation is to carry explicit structured command fields:

- `kind`
- `command`
- `args`
- `raw_input`

## Session Semantics

Slash commands should still participate in sessions and turns.

- they should carry `session_id`
- they should receive a `turn_id`
- they should be recorded in the session timeline
- they may or may not create a full agent reasoning loop

That keeps traceability consistent while still allowing short-circuit execution.

## Recommended Initial Command Set

### MVP deterministic commands

- `/ping`
  - health/smoke path
  - reply: `pong`
- `/help`
  - list available commands for the current channel/session
- `/status`
  - report session status and active turn state

### Near-term operational commands

- `/pause`
- `/resume`
- `/fork`
- `/checkpoint`
- `/preapprove`
- `/approval status`
- `/approval reset`

### Later agent-assisted commands

- `/plan`
- `/summarize`
- `/search`

## Proposed Execution Rules

### `/ping`

- handled directly by `agent-core`
- no model call
- writes session checkpoints as normal
- completes the current task immediately
- emits a final reply payload

This is the ideal first implementation because it validates:

- transport to agent routing
- session/turn correlation
- checkpoint behavior
- final reply path
- binary smoke harness

## Testing Recommendation

Use `/ping` as the binary smoke-test command.

Why:

- deterministic
- no network dependency on model providers
- still exercises real `ansible` and `agent-core` binaries
- validates the short-circuit command path

This should become the standard local smoke flow before we rely on full model-backed round trips.

## Full Recommendation

- add slash commands as a first-class system capability
- parse them before the normal LLM loop
- keep them session-aware and turn-aware
- route deterministic commands locally
- reserve model invocation only for agent-assisted commands
- use `/ping` as the first command and as the default binary smoke-test path

For approval-specific command semantics, see [APPROVAL_UX_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/APPROVAL_UX_PROPOSAL.md).
