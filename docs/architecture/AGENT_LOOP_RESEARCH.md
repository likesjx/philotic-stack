---
title: "Agent Loop Research Notes"
doc_type: reference
domain: runtime-sessions
status: active
last_updated: 2026-03-12
tags:
  - research
  - agent-loop
  - references
  - runtime
related_docs:
  - ARCHITECTURE_STATUS.md
  - AGENT_LOOP_PROPOSAL.md
  - PHILOTIC_AGENT_LOOP_SPEC.md
task_refs:
  - docs/task.md
---

# Agent Loop Research Notes

## Goal

Before implementing the full Philotic agent loop, compare the strongest current patterns from Claude, OpenAI, and LangGraph-style durable runtimes and extract the pieces worth copying.

## What the Major Systems Are Doing

### Pi / `pi-agent-core` (the OpenClaw / ZeroClaw core)

This is the most directly relevant reference because it is the actual loop lineage behind OpenClaw and ZeroClaw.

Key observations from `pi-agent-core`:

- The loop is deliberately minimal:
  - prompt
  - stream assistant response
  - execute tool calls
  - append tool results
  - loop again if tools were called
- It keeps `AgentMessage` as the internal representation and only converts to provider-specific LLM messages at the model boundary.
- It has a clean event model:
  - `agent_start`
  - `turn_start`
  - `message_start` / `message_update` / `message_end`
  - `tool_execution_start` / `tool_execution_update` / `tool_execution_end`
  - `turn_end`
  - `agent_end`
- It supports `transformContext(...)` before each LLM call.
- It supports mid-run steering and post-run follow-up queues.
- It does **not** provide durable orchestration by itself; it is an in-memory turn engine.

Useful source points:

- The package README describes the loop as:
  - `AgentMessage[] -> transformContext() -> AgentMessage[] -> convertToLlm() -> Message[] -> LLM`
  - and shows the event sequence for plain turns and tool-call turns.  
  Source: [pi-agent-core README](https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/agent/README.md)
- The core loop in `agent-loop.ts` has:
  - an outer loop for follow-up work
  - an inner loop for assistant response -> tool execution -> steering injection
  - tool execution after each assistant message
  - follow-up message injection when the agent would otherwise stop.  
  Source: [pi-agent-core agent-loop.ts](https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/agent/src/agent-loop.ts)
- The types explicitly model:
  - `transformContext`
  - `convertToLlm`
  - `getSteeringMessages`
  - `getFollowUpMessages`
  - `AgentTool`
  - `AgentState` with `pendingToolCalls`, `streamMessage`, and `messages`.  
  Source: [pi-agent-core types.ts](https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/agent/src/types.ts)

What Pi gets very right:

- minimal, comprehensible loop semantics
- provider boundary separation via `convertToLlm`
- context transformation hook before each model call
- first-class event streaming
- steering/follow-up without inventing a giant planning abstraction

What Pi does not solve for Philotic:

- durable session ownership
- checkpoint/recovery
- graph-owned session state
- approval interrupts as persisted runtime state
- cross-component orchestration

So the right move for Philotic is not "replace Pi."
It is:

- keep Pi's clean turn engine ideas
- wrap them in Philotic durability, sessions, approvals, and cross-component coordination

### Anthropic Claude Agent SDK

The Claude Agent SDK is explicitly built around a streaming agent loop where the runtime yields intermediate steps while the SDK handles orchestration.

Key observations:

- The loop is eventful, not monolithic.
- Tool calls and tool results are first-class outputs in the stream.
- The runtime handles orchestration, retries, context management, and permissions.
- Plan mode is a distinct execution mode, not just a prompt trick.
- Slash commands are treated as user convenience, not new agent capabilities.

Useful source points:

- Anthropic says the `async for` loop "keeps running as Claude thinks, calls tools, observes results, and decides what to do next," and that "the SDK handles the orchestration (tool execution, context management, retries)."  
  Source: [Claude Agent SDK quickstart](https://platform.claude.com/docs/en/agent-sdk/quickstart)
- Anthropic's tool-use docs treat `tool_use` and `tool_result` blocks as strict message structure, and explicitly call out `pause_turn` for long-running turns.  
  Source: [How to implement tool use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/implement-tool-use)
- Anthropic's cookbook notes that slash commands are "syntactic sugar for users, not new agent capabilities."  
  Source: [Chief of Staff Agent cookbook](https://platform.claude.com/cookbook/claude-agent-sdk-01-the-chief-of-staff-agent)

### OpenAI Responses / Agents SDK

OpenAI has moved hard toward the Responses API as the canonical agentic interface.

Key observations:

- The API itself is now the loop substrate.
- Tool calls are item-based, not just implicit message content.
- Multi-turn agent loops work better when reasoning/tool items are preserved across turns.
- The Agents SDK emphasizes tracing and handoffs, not just one model making one reply.

Useful source points:

- OpenAI says the Responses API is "agentic by default" and can call multiple tools "within the span of one API request."  
  Source: [Migrate to the Responses API](https://developers.openai.com/api/docs/guides/migrate-to-responses)
- OpenAI recommends `store: true` and carrying reasoning items forward for best multi-step function-calling behavior.  
  Source: [Reasoning best practices](https://developers.openai.com/api/docs/guides/reasoning-best-practices)
- OpenAI positions its Agents SDK around using tools, handoffs, streaming partials, and "a full trace of what happened."  
  Source: [Agents SDK](https://developers.openai.com/api/docs/guides/agents-sdk)

### LangGraph / Durable Graph Runtimes

LangGraph is the clearest public model for durable, resumable, human-in-the-loop execution.

Key observations:

- Every meaningful step should be checkpointed.
- Human approval is implemented as a real interrupt/resume primitive.
- Persistence is thread-scoped and powers replay, time travel, HITL, and fault tolerance.

Useful source points:

- LangGraph says the checkpointer saves a checkpoint "at every super-step" and that this enables "human-in-the-loop, memory, time travel, and fault-tolerance."  
  Source: [LangGraph persistence](https://docs.langchain.com/oss/javascript/langgraph/persistence)
- LangGraph interrupts pause execution, persist state, and resume later with external input.  
  Source: [LangGraph interrupts](https://docs.langchain.com/oss/python/langgraph/interrupts)

## What This Means for Philotic

The best approach for Philotic is **not** to clone one vendor loop wholesale.

The best approach is:

- keep session and turn durability in Philotic's graph/session substrate
- keep the cognitive loop in `agent-core`
- model loop steps explicitly as state transitions
- checkpoint at step boundaries
- treat tool calls, tool results, pauses, and approvals as first-class events

The closest spiritual template is now:

- Pi for the core loop shape
- Anthropic/OpenAI for structured provider/tool interaction patterns
- LangGraph for durability and interrupts

That gives us the best parts of all three systems without surrendering our architecture to provider-specific assumptions.

## Recommended Philotic Loop Shape

### Phase 0: Intake

- Resolve `session_id`
- Create `turn_id`
- Check slash-command short-circuit
- Load canonical session snapshot

### Phase 1: Working Context Build

- Build turn working set from:
  - session snapshot
  - recent turns
  - apartment checkpoint projection
  - effective toolset / skillset / workspace / model-controller

### Phase 2: Planning / Model Step

- Ask the model for the next action, not just "the final answer"
- The next action should be one of:
  - `respond`
  - `tool_call`
  - `request_approval`
  - `handoff`
  - `fail`

This is the key design move: make the loop action-based, not reply-based.

### Phase 3: Execute / Pause / Observe

- If `tool_call`
  - validate
  - checkpoint
  - execute tool
  - append tool result
  - continue loop
- If `request_approval`
  - checkpoint
  - emit waiting state
  - pause turn until resumed
- If `respond`
  - finalize turn
  - checkpoint
  - emit final reply

### Phase 4: Bound and Finalize

- enforce max iterations
- enforce tool policy
- fail gracefully when limits are exceeded

## Concrete Philotic Recommendations

### 1. Use an explicit loop state machine

Recommended turn states:

- `queued`
- `loading_context`
- `thinking`
- `waiting_tool`
- `waiting_approval`
- `resuming`
- `completed`
- `failed`

Recommended step actions:

- `respond`
- `tool_call`
- `request_approval`
- `handoff`
- `fail`

### 2. Checkpoint after every super-step

Philotic should copy LangGraph here.

Checkpoint after:

- context load
- each model step
- each tool execution
- each approval interruption
- finalization

This is the right answer for:

- crash recovery
- debugging
- replay
- future forks

### 3. Preserve item/block semantics, not just flat chat text

Philotic should copy the provider-neutral lesson from Anthropic/OpenAI here.

Internally, a turn should preserve structured records like:

- user message
- assistant reasoning summary
- tool request
- tool result
- approval request
- final response

Do not reduce everything to one assistant transcript blob.

### 4. Keep provider-specific reasoning details optional

Philotic should **not** depend on provider-private reasoning formats.

But when a provider supports useful structured continuation state:

- preserve it
- carry it forward when helpful
- avoid making it the canonical session representation

This is closest to OpenAI's "keep reasoning items adjacent to tool loops" guidance without coupling the graph model to one API.

### 5. Make approvals real interrupts

Philotic should copy LangGraph here directly in spirit.

Approvals should not be "try again later with a prompt."

They should be:

- explicit interrupt state
- persisted
- resumable with the same `session_id` / `turn_id`

### 6. Keep slash commands outside the loop unless needed

Philotic should keep:

- deterministic `/commands` as short-circuits
- agent-assisted commands as structured entries into the loop

That matches the Anthropic cookbook's framing and keeps operational latency low.

## What Not To Copy

### Don't fully hide the loop inside a provider SDK

Claude and OpenAI both offer higher-level orchestration, but Philotic needs:

- canonical session state in our graph
- our own recovery semantics
- our own approval model
- cross-component coordination

So the provider loop can inform us, but it should not own us.

### Don't make turns just linear text replay

That is too weak for:

- approvals
- tool retries
- resumability
- forks
- tracing

### Don't make every message a full agent loop

Slash commands and some operational actions should bypass it.

## Full Recommendation

Philotic should implement:

- a **Philotic-owned, explicit turn state machine**
- with **provider-informed step semantics**
- **checkpointed after every meaningful step**
- **interrupt/resume for approval**
- **structured tool/action records**
- **slash-command short-circuiting before the loop**

In short:

- Pi gives us the minimal turn-engine skeleton already proven in the OpenClaw line
- Anthropic gives us the shape of a practical streaming tool loop
- OpenAI reinforces item-based state and preserving reasoning adjacency
- LangGraph gives us the best durability/interruption model

The best Philotic loop is:

- **Claude-style in interaction**
- **OpenAI-style in item/action structure**
- **LangGraph-style in durability**
- **Philotic-style in ownership**

## Sources

- [Claude Agent SDK quickstart](https://platform.claude.com/docs/en/agent-sdk/quickstart)
- [pi-agent-core README](https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/agent/README.md)
- [pi-agent-core agent-loop.ts](https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/agent/src/agent-loop.ts)
- [pi-agent-core types.ts](https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/agent/src/types.ts)
- [Anthropic tool use guide](https://platform.claude.com/docs/en/agents-and-tools/tool-use/implement-tool-use)
- [Anthropic Chief of Staff agent cookbook](https://platform.claude.com/cookbook/claude-agent-sdk-01-the-chief-of-staff-agent)
- [OpenAI migrate to Responses](https://developers.openai.com/api/docs/guides/migrate-to-responses)
- [OpenAI reasoning best practices](https://developers.openai.com/api/docs/guides/reasoning-best-practices)
- [OpenAI Agents SDK guide](https://developers.openai.com/api/docs/guides/agents-sdk)
- [LangGraph persistence](https://docs.langchain.com/oss/javascript/langgraph/persistence)
- [LangGraph interrupts](https://docs.langchain.com/oss/python/langgraph/interrupts)
