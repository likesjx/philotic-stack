---
title: Whisper Protocol — Silent Reactive Role Dispatch via Lookaside Turn
doc_type: proposal
domain: cognitive-plane
status: proposed
disposition: implemented
last_updated: 2026-04-01
tags:
- role-handoff
- whisper
- lookaside
- dispatch
- membrane
- attribution
- silent-handoff
related_docs:
- COGNITIVE_LOOP_PROPOSAL.md
- AGENT_WORKFLOW_PROPOSAL.md
- APPROVAL_UX_PROPOSAL.md
- CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md
proposal_id: whisper-protocol
implements:
- cognitive-loop
implemented_by: []
active_seams:
- role-handoff-seam
---

# Whisper Protocol — Silent Reactive Role Dispatch via Lookaside Turn

## Goal

Enable seamless, low-friction transitions between the Orchestrator and Specialist
roles via silent reactive triggers — without interrupting the conversation or
requiring the user to know a role change is happening.

The current role handoff model is modal: the user explicitly invokes `/role <name>`
or the agent asks permission to hand off. This is correct for deliberate role changes
but too heavy for *reactive* specialist delegation — cases where the Orchestrator
detects a specialist context and wants a specialist answer incorporated inline.

The Whisper Protocol introduces a **lookaside turn** primitive that makes this
lightweight, fire-and-forget, and multi-purpose.

## Core Mechanism: Lookaside Turn

A **lookaside turn** is a secondary model-request envelope that a philote emits
*alongside* (not instead of) its normal turn completion. The delegating philote A:

1. Completes its own turn normally — reply to membrane goes out as usual.
2. Optionally also emits a `paracrine_request` task to a target philote B.
3. A does **not** wait for B. The turn is done.
4. B processes the lookaside — using its own model, tools, context.
5. B's response arrives back at A (or directly at membrane) as an `action: "paracrine_response"` inbound task.
6. When A receives a `paracrine_response`, its model handles it with a dedicated skill/toolset context: merge into next reply, forward to membrane, escalate, etc.

### Why fire-and-forget?

- A's turn completes immediately — no blocking, no timeout risk.
- B can take as long as needed, use tools, even do its own lookaside.
- Heartbeat/retry loops are possible: B can re-ask A for status on a running thread.
- High-priority inbound messages to A can preempt the lookaside queue.

### Lookaside is a general primitive

The same mechanism serves multiple purposes:

| Use case | Description |
|---|---|
| **Whisper delegation** | Orchestrator consults a Specialist without visible handoff |
| **Mid-turn update** | Philote sends an intermediate progress note to membrane while a long task runs |
| **Peer delegation** | Any philote delegates to any other philote, not just O→S |
| **Heartbeat** | Lookaside asks a peer for a status ping; peer responds via paracrine_response |
| **High-priority inbound** | Another philote sends an urgent request that arrives as paracrine_response |

## The `delegate.whisper` Tool

`delegate.whisper` is the user-facing name for the lookaside primitive when used
for Orchestrator→Specialist delegation. It is:

- A **tool registered in philote's toolset/skill keyring** — not a hotel operator
  command and not available to external users.
- Gated by skill assignment: the Orchestrator role must have `delegate.whisper`
  in its allowed toolset for it to be callable.
- Executed by **tool-runner** (like all philote tools): tool-runner receives the
  `execute_tool` task, dispatches `IpcRequest::ParacrineEmit` to the hotel, and
  returns `"dispatched"` as the tool result immediately — no waiting.

```json
{
  "tool": "delegate.whisper",
  "arguments": {
    "role": "theoretician",
    "prompt": "Explain the CAP theorem implications for the session checkpoint LWW design.",
    "reply_to": "self"
  }
}
```

`reply_to` values:
- `"self"` — B's response arrives at A as a `paracrine_response`
- `"membrane"` — B's response goes directly to the transport (Telegram, etc.)
- `"<node_id>/<role>"` — explicit routing to another philote or component

## Protocol Phases

### Phase 1 — Emit (Orchestrator/Delegating Philote A)

A calls `delegate.whisper` as a tool. Tool-runner dispatches to the hotel via
`IpcRequest::ParacrineEmit { role, exosome, reply_to_node, reply_to_role }`.
The hotel routes the `paracrine_request` task to B's inbox (materializing B if
needed via the parking lot). Hotel returns `Standard { ok: true }` immediately.
A's turn completes normally.

### Phase 2 — Process (Specialist/Delegate Philote B)

B receives an `action: "paracrine_request"` inbound task. It is routed through
`handle_user_message` — identical to `peer.delegate`. B's model processes the
prompt, may use tools, and generates a response.

B emits its response to the `reply_to` address set by A. Before emitting, B
appends an `@agent:role_name` attribution tag to the end of its response content:

```
The CAP theorem implies...

@agent:theoretician
```

The tag is appended automatically whenever `PHILOTIC_ROLE_NAME` is set in the
specialist's environment — no persona prompt change needed.

### Phase 3 — Return & Attribution (Specialist B → reply_to)

B emits via the normal `FinalReplyPayload → EmitTask` path to the `reply_to`
address.

**If reply_to is membrane**: the membrane interceptor parses `@agent:<role>` at the
end of outbound message content, strips the tag, and attaches a transport-appropriate
affordance:
- **Telegram**: Inline button `[ 🎭 Switch to <Role Name> ]`
- **Discord**: Ephemeral button component
- **CLI**: Trailing hint `(answered by theoretician — /role theoretician to switch)`

**If reply_to is A (self)**: arrives at A as `action: "paracrine_response"` inbound
task. A's model handles it with a dedicated skill/toolset context. The model can:
- Incorporate B's answer into a follow-up reply to the user
- Forward to membrane directly
- Ignore / log and continue
- Trigger another lookaside

### Phase 4 — Activation (User)

If the user clicks the inline button (on a membrane-delivered B response or a
merged A+B response), the membrane sends `/role <role_name>` back to the agent.
This triggers the existing modal role handoff — the specialist becomes the active
persona.

The Whisper Protocol completes the handoff **only when the user asks for it**.

## Attribution Tag Contract

Format: `@agent:<role_name>` — at the very end of the content, on its own line,
no trailing whitespace.

- Role names: lowercase alphanumeric + dash/underscore only.
- Appended automatically by specialist philotes when `PHILOTIC_ROLE_NAME` is set.
- Always present on responses from named specialist roles.
- Absent on Orchestrator responses (no `PHILOTIC_ROLE_NAME`).
- Membrane strips the tag before delivery; attaches affordance if specialist role
  is known.

## Implementation Seams

### `Exosome` + `IpcRequest::ParacrineEmit`

```rust
/// The vesicle — packages the message payload for paracrine delivery.
pub struct Exosome {
    pub prompt: String,
    pub context: Option<serde_json::Value>,  // session excerpt, tool results, etc.
    pub lookaside_id: Option<String>,         // correlation ID echoed in paracrine_response
}

/// The dispatch verb — fire-and-forget paracrine emit.
ParacrineEmit {
    role: String,              // target role name
    exosome: Exosome,          // the message envelope
    reply_to_node: String,     // node to route B's response to
    reply_to_role: String,     // role at that node ("membrane", "agent", etc.)
    timeout_secs: Option<u64>, // materialisation timeout; None = hotel default
}
```

Hotel handling: delivers `paracrine_request` to target role inbox (parking lot
if not materialized); returns `Standard { ok: true }` immediately to caller.
Task JSON includes both `content` (for `normalized_user_content` compat) and
`exosome` (structured envelope).

### tool-runner: `delegate.whisper`

In `execute_tool` dispatch:
- `delegate.whisper` → builds `Exosome { prompt, .. }`, calls `IpcRequest::ParacrineEmit`
- Returns `"paracrine dispatched"` as `result_content`
- `reply_to` defaults to `"self"` (A's node + agent role) if not specified

### philote/runtime: `paracrine_request` inbound

In `run()` dispatch loop — new arm:
```
action == "paracrine_request" → handle_user_message(task, task_id)
```
Identical to `peer.delegate` routing. `final_reply_to/final_reply_role` are set
by the `ParacrineEmit` call to route the response to the correct destination.

### philote/runtime: Attribution tag

In `deliver_text_reply`, before constructing `FinalReplyPayload`:
```rust
if let Ok(role_name) = std::env::var("PHILOTIC_ROLE_NAME") {
    if !role_name.is_empty() {
        content = format!("{}\n\n@agent:{}", content, role_name);
    }
}
```
This applies to ALL responses from named specialist philotes — not just lookaside
turns. The membrane always has provenance.

### philote/runtime: `paracrine_response` inbound

In `run()` dispatch loop — new arm:
```
action == "paracrine_response" → handle_paracrine_response(task, task_id)
```

`handle_paracrine_response` routes to the model with a dedicated skill/toolset
context (defined per-role on the skill keyring). The model sees B's response and
the original turn context and decides how to proceed.

### Membrane Interceptor

New interceptor stage in the outbound message pipeline (before transport serialization):

```rust
fn lookaside_interceptor(content: &str) -> (String, Option<String>) {
    // Matches `@agent:<role_name>` at end of content
    // Returns (clean_content, Option<role_name>)
}
```

If `role_name` is `Some`, membrane constructs the inline button and attaches it
to the message envelope before sending.

### Modal Activation Path

The inline button triggers a `/role <name>` synthetic command. This reuses the
existing role handoff path — no new state machine.

## What This Is Not

- **Not synchronous**: A does not block waiting for B. The lookaside is always
  fire-and-forget from A's perspective.

- **Not automatic mode-switching**: The user must click the inline button to enter
  the specialist's context. The protocol never changes the active persona without
  user intent.

- **Not limited to Orchestrator→Specialist**: Any philote can use the lookaside
  primitive once `delegate.whisper` is in its toolset.

- **Not a streaming path**: Phase 1 lookaside requests are single model turns.
  Streaming lookaside (interleaved with A's turn) is a future consideration.

## Open Questions

- **Lookaside response skill context**: What specific tools/skills should be
  available when A's model processes a `paracrine_response`? Likely a minimal
  set: `reply_to_membrane`, `merge_with_active_turn`, `discard`.

- **Multiple in-flight lookaides**: Can A have >1 pending lookaside at a time?
  Probably yes — they arrive as separate `paracrine_response` tasks. Correlation
  via `turn_id` or a `lookaside_id` field.

- **Whisper loops**: Can B itself call `delegate.whisper`? Probably yes if B has
  the tool in its keyring — but depth should be bounded to avoid recursive
  materialization storms.

- **Attribution on merged replies**: If A incorporates B's answer into its own
  response, how does the attribution tag surface? A could forward the tag, or
  membrane could detect B's `@agent:role` in the merged content.

## Phases

### Phase 1 — Fire-and-forget lookaside + attribution tag *(target: next sprint)*

1. Add `IpcRequest::ParacrineEmit` to philotic-client
2. Hotel handler: deliver to target role, return immediately
3. tool-runner: add `delegate.whisper` execution path
4. philote/runtime: `paracrine_request` arm in dispatch loop
5. philote/runtime: `@agent:role` auto-append in `deliver_text_reply` when `PHILOTIC_ROLE_NAME` is set
6. Validate: Orchestrator calls `delegate.whisper`, lookaside arrives at specialist, specialist responds with `@agent:role` tag

### Phase 2 — `paracrine_response` handling + membrane affordance

1. philote/runtime: `paracrine_response` arm + `handle_paracrine_response`
2. Membrane interceptor: parse `@agent:<role>` from outbound messages
3. Strip tag, attach Telegram inline button
4. Button triggers `/role <name>` synthetic command
5. Validate: end-to-end — whisper dispatch → specialist response → `@agent:role` → membrane button → click → modal role handoff

### Phase 3 — Multi-lookaside, heartbeat, peer patterns

1. Multiple in-flight lookaside correlation via `lookaside_id`
2. Heartbeat pattern: B periodically pings A with status
3. Peer-to-peer delegation (any philote → any philote)
4. Lookaside tool calls (B can fan out further lookaides)
5. Streaming lookaside (interleaved with A's main turn)
