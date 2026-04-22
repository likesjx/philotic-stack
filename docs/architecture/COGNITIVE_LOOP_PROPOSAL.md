---
title: Cognitive Loop Architecture
doc_type: proposal
domain: runtime-sessions
status: accepted-current-slice
last_updated: 2026-03-31
tags:
- agent-loop
- context-envelope
- memory
- plan-execute-evaluate
- streaming
- rules
- settings
related_docs:
- AGENT_LOOP_PROPOSAL.md
- PHILOTIC_AGENT_LOOP_SPEC.md
- PERSISTENCE_TIERS_PROPOSAL.md
- MUNINN_MEMORY_PROTOCOL_PROPOSAL.md
- PHILOTE_MEMORY_CORE_PROPOSAL.md
- GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md
- ARCHITECTURE_STATUS.md
task_refs:
- docs/task.md
proposal_id: cognitive-loop-architecture
active_seams:
- context-envelope-contract
- memory-local-tools
- active-plan-streaming
- rules-tier
---

# Cognitive Loop Architecture

## Goal

Redesign the philote cognitive loop to be structurally correct, observable, and
memory-coherent across turns, restarts, and role transitions.

The previous `AGENT_LOOP_PROPOSAL.md` closed four execution gaps (tool re-entry,
media routing, tool catalog, approval granularity). This proposal addresses the
deeper architectural layer: **how context is assembled, how memory flows, how
multi-step plans are represented, and how the agent surfaces its work in real time**.

## Disposition

`accepted for current slice`

---

## Core Recommendation

Five interlocking changes to philote and model-router:

1. **Context envelope contract** — always-populated, call-type governs which sections model-router renders
2. **Memory architecture** — two-axis context: recency (local rolling window) + relevance (Muninn, on-demand)
3. **Plan-Execute-Evaluate loop** — `active_plan` as a first-class context section; model-produced, runtime-captured
4. **Streaming turn events** — ephemeral progress visible to the user; final answer replaces scaffolding
5. **Rules tier** — durable behavioral constraints elevated from `CognitiveOutcome`; never compacted

---

## 1. Context Envelope Contract

### Problem

philote sends structurally different `ModelRequestPayload` objects on initial turns vs. re-entry
after tool results. Re-entry drops `context`, `context_projection`, and `response_contract` — putting
everything into the raw `prompt` field. model-router's `composed_prompt_text()` takes a materially
different code path, the Gemini schema changes, and the model operates without its identity or
prior turn history.

### Design

**Invariant**: every `generate_text` request from philote carries the same structural envelope:

```
context:             Some(ContextEnvelope)   — always rebuilt from session state
context_projection:  Some(ContextProjection) — always
response_contract:   Some(...)               — always, for cognitive calls
```

**Sections** — always populated (empty if nothing to show):

| Section | What | Changes per turn? |
|---|---|---|
| `identity` | Agent persona, soul text | Never within session |
| `instructions` | Role addendum, skill grants, rules | On binding change |
| `memory` | Rolling local window (last M memories) | Per turn |
| `dialogue_window` | Rolling recent turns (time + token budget) | Per turn |
| `active_turn` | Current user message | Every turn |
| `tool_history` | In-flight (call, result) pairs this turn | Grows mid-turn |
| `active_plan` | Structured plan if model produced one | Grows mid-turn |

**Call-type section matrix** — model-router applies this, not philote:

| | identity | instructions | memory | dialogue_window | active_turn | tool_history | active_plan |
|---|---|---|---|---|---|---|---|
| cognitive (initial) | ✅ | ✅ | ✅ | ✅ | ✅ | — | — |
| cognitive (re-entry) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| transcribe | — | — | — | — | — | — | — |
| analyze_media | — | ✅ | — | — | ✅ | — | — |
| synthesize | — | — | — | — | — | — | — |
| embed | — | — | — | — | — | — | — |

**`build_context_envelope()`** replaces both `model_context_from_projection` (initial)
and `build_reentry_prompt` (re-entry). Called on every cognitive turn. `tool_history` and
`active_plan` are populated when non-empty; empty otherwise. The raw `prompt` field is
deprecated as a primary path.

### Dialogue Window

Rolling window bounded by **both** time and token budget. philote filters `recent_turns`
to the window before building the envelope. Configurable via `AgentSettings.context_window`.

Default: **10 minutes / 10k tokens**.

Turn entries in `dialogue_window` include:
- `role: "user"` — user message text
- `role: "assistant"` — final response text + tool call names/args (not results)
- `concept` — memory concept slug if present (self-describing entry)

Tool results are **not** stored in `dialogue_window`. Only what was said and what was done.

---

## 2. Memory Architecture

### Two-Axis Context Strategy

**Recency axis** → `memory` section (local rolling window)
- Source: session state list of recent `(concept, summary, timestamp)` tuples
- Updated on turn complete when `memory_concept` is present
- No Muninn call at turn start — in-process, zero latency
- Default: last **10 entries**
- Rolls off by count; persists across session restart via apartment checkpoint

**Relevance axis** → `memory.recall` tool
- Model calls `memory.recall(query, limit?)` when the rolling window doesn't have what it needs
- philote intercepts as a **local agent tool** (never dispatched to tool-runner)
- Calls `engine.activate(query, MemoryScope::SelfOnly, limit)` on the in-process `MuninnRestEngine`
- Returns formatted engram list as tool result
- Model decides when to go deeper — philote does not do per-turn semantic recall automatically

### Memory Write Path

Model produces `memory_concept` (slug) and `memory_summary` (1–3 sentence synthesis) as
response channels. On turn complete, philote fires:

```rust
engine.remember(MemoryScope::SelfOnly, &concept, &summary, tags)  // fire-and-forget
```

Tags include: `[agent_id, "turn", ...tool names used...]`.

Model can also write explicitly mid-turn via `memory.remember(concept, content, tags?)` tool.
philote intercepts, spawns async write, returns `"Memory recorded."` immediately.

### Role Incarnation and Memory Scope

`MemoryScope::SelfOnly` resolves to `self:{agent_id}`. Agent ID is fixed regardless of active
role incarnation. All roles of the same agent access the same vault — role does not partition
memory ownership.

`AttentionalLens` shapes retrieval bias per role (include/exclude tags, recency/frequency weights)
without changing the underlying vault. Set via `engine.set_lens()` on role activation.

### Local Agent Memory Tools

```
memory.recall(query, limit?)
  → engine.activate(query, SelfOnly, limit)
  → formatted engram list
  class: "memory", approval: false

memory.remember(concept, content, tags?)
  → engine.remember(SelfOnly, ...) — async, fire-and-forget
  → "Memory recorded."
  class: "memory", approval: false
```

Both handled by philote directly. Never routed to tool-runner.

---

## 3. Plan-Execute-Evaluate Loop

### Problem

The current loop is purely reactive — model sees tool result, decides next action. There is no
explicit plan, no evaluation step, and no observable progress. The "Ralph Wiggum" failure mode
(loop runs, tool calls fire, no progress toward goal) has no detection mechanism. The iteration
cap is the only guard.

### Design

**Plan phase** — model produces a structured plan as its first output when:
- A skill is activated (always required)
- The task is multi-step (model judges this)

Plan shape:
```json
{
  "goal": "summarize README and check build system",
  "steps": [
    { "index": 1, "description": "read README.md", "tool": "workspace.read", "status": "pending" },
    { "index": 2, "description": "list project root", "tool": "workspace.list", "status": "pending" },
    { "index": 3, "description": "synthesize findings", "tool": null, "status": "pending" }
  ]
}
```

philote captures this as `active_plan` in `WorkingTurn` and emits it immediately to membrane
(user sees the plan before any tool fires).

**Execute phase** — existing tool loop, enhanced:
- Each step completion updates `active_plan.steps[n].status` → `"done"` or `"failed"`
- philote emits `step_completed` / `step_failed` turn event
- model sees updated plan in context on each re-entry

**Evaluate phase** — after each tool result the model explicitly assesses:
- Can I respond now? → `Respond`
- Need another step? → `ToolCall` (continues loop)
- Stuck / goal not achievable? → `Fail` with honest status + what was tried

**Ralph Wiggum detection** — if N consecutive steps fail or the model calls the same tool
with identical arguments twice, philote surfaces to the user rather than continuing to iterate.
Default threshold: 3 consecutive failures.

### `active_plan` as a Context Section

Included in the cognitive re-entry envelope when non-empty. model-router renders it as:

```
[Active plan]
Goal: summarize README and check build system
Steps:
  [✓] 1. read README.md
  [ ] 2. list project root
  [ ] 3. synthesize findings
```

Model sees its own plan on each re-entry, tracks progress, evaluates honestly.

---

## 4. Streaming Turn Events

### Design

philote emits turn events to membrane throughout execution. Membrane renders these as
ephemeral status updates (Telegram: edit-in-place). Final answer replaces the scaffolding.

Extended event types:

```
plan_ready         { plan: ActivePlan }
step_started       { step_index, description, tool_name? }
step_completed     { step_index, result_summary }
step_failed        { step_index, error }
tool_dispatched    { tool_name, arguments_summary }
loop_recovering    { checkpoint_summary }
```

Existing events retained:
```
waiting_model, thinking, waiting_tool, waiting_approval, waiting_voice
```

### Interrupt Handling

**Normal message arrives mid-loop** → queued. When active turn completes, multiple queued
messages are merged into a single coherent turn.

**Message with steering flag** → injected into the next model iteration of the active turn,
same as `/approve note` behavior. Does not interrupt the current tool execution.

---

## 5. Rules Tier

### Problem

Memories decay. Compacted turns lose nuance. Some behavioral constraints must survive
indefinitely and must not be muddied by summarization or rolling off. The current system has
no durable rules layer — everything either lives in prompt text (soul/identity, which can be
overwritten) or in Muninn (which decays).

### Design

**Three persistence tiers** for cognitive state:

| Tier | What | Decays? | Always in prompt? | Owner |
|---|---|---|---|---|
| Working | tool_history, active_plan, current turn | Turn-scoped | Yes, while active | Session state |
| Memory | Engrams — facts, beliefs, observations | Yes (ACT-R) | No — recalled on demand | Muninn |
| Rules | Behavioral constraints, elevated beliefs | No | Yes — always in `instructions` | Context graph |

Rules live in the context graph as `RuleRecord` entries, owned by the hotel. Injected into
the `instructions` section of every cognitive call. Never rolled off by the dialogue window.

### CognitiveOutcome → Rule Elevation

`CognitiveOutcome` types in `memory_core` define what's memory-eligible at turn end:
- `SolidifiedBelief` — a belief that crystallized during reasoning
- `ResolvedContradiction` — a contradiction resolved
- `RejectedApproach` — an approach tried and abandoned
- `MetacognitiveObservation` — an observation about cognitive patterns

Elevation pathway — **operator-confirmed only**:
```
SolidifiedBelief (high confidence, confirmed by operator)  →  Rule
RejectedApproach (recurring, structural)                   →  Rule
Operator correction (explicit "always do X")               →  Rule immediately
```

Model proposes via `rule.propose(description, rationale)` tool. Hotel gates on operator
approval. Agent cannot self-elevate beliefs into rules without human confirmation — this
gate prevents entrenchment of bad behavior.

`rule.propose` class: `"config"`, approval: always required (bypasses preapproval).

---

## 6. Settings Tree

See `AGENT_SETTINGS_CATALOG.md` for the full catalog with descriptions, types, defaults,
valid ranges, and which call types each setting affects.

### Schema

```rust
pub struct AgentSettings {
    pub context_window: ContextWindowPolicy,
    pub memory: MemoryPolicy,
    pub execution: ExecutionPolicy,
}

pub struct ContextWindowPolicy {
    pub dialogue_window_minutes: u32,    // default: 10, min: 2, max: 60
    pub dialogue_window_tokens: usize,   // default: 10_000, min: 1_000, max: 50_000
    pub include_tool_calls: bool,        // default: true — names+args in dialogue_window
}

pub struct MemoryPolicy {
    pub memory_window_size: usize,       // default: 10, min: 3, max: 30
    pub long_term_recall_enabled: bool,  // default: true — memory.recall available
    pub recall_limit: usize,             // default: 5, min: 1, max: 20
}

pub struct ExecutionPolicy {
    pub iteration_cap: u32,              // default: 10, min: 1, max: 50
    pub plan_required_on_skill: bool,    // default: true
    pub stream_tool_events: bool,        // default: true
    pub stall_detection_threshold: u32,  // default: 3 consecutive failures
}
```

Stored in context graph, keyed by `agent_id`. Fetched at session init alongside agent profile.
Agents modify via `agent.configure` — config path prefix: `settings.*`.

---

## Crash Recovery

If `active_turn` is present in the apartment checkpoint on restart with phase
`WaitingTool` or `WaitingModel`:

1. philote emits `loop_recovering` event to membrane
2. Builds a recovery cognitive turn:
   - Full context envelope rebuilt from checkpoint
   - `tool_history` from checkpoint
   - `active_plan` from checkpoint (if present)
   - System note: "You were interrupted mid-task. Here is what was completed. Continue or start over."
3. Re-enters the loop from checkpoint state

The `active_plan` section is what makes recovery clean — the model wakes up seeing exactly
where it was and what remains.

---

## Current Slice

### Slice 1 — Context Envelope Fix (unblocks everything else)
- `build_context_envelope()` replaces `build_reentry_prompt` and `model_context_from_projection`
- All cognitive calls send full structured envelope
- `tool_history` and `active_plan` added as context sections
- model-router `composed_prompt_text()` gains per-TaskKind section inclusion rules
- `prompt` field deprecated as primary path

### Slice 2 — Settings Tree
- `AgentSettings`, `ContextWindowPolicy`, `MemoryPolicy`, `ExecutionPolicy` structs
- `AGENT_SETTINGS_CATALOG.md` — full catalog doc
- `agent.configure` config paths expanded to `settings.*`
- Dialogue window filtering in `build_context_envelope()`

### Slice 3 — Memory Local Tools
- `memory.recall` local agent tool → `engine.activate()`
- `memory.remember` local agent tool → `engine.remember()` fire-and-forget
- Both added to tool catalog with class `"memory"`
- `memory_summary` response channel added to Gemini schema

### Slice 4 — Active Plan + Streaming
- `active_plan: Option<ActivePlan>` added to `WorkingTurn`
- Plan capture in `handle_model_response` when plan JSON is present
- Extended turn events: `plan_ready`, `step_started`, `step_completed`, `step_failed`
- Stall detection: N consecutive failures → surface to user

### Slice 5 — Rules Tier
- `RuleRecord` in context graph alongside `AbstractToolRecord`
- `upsert_rule` / `list_rules` on `GraphStorage`
- `rule.propose` tool — class `"config"`, always requires operator approval
- Rules injected into `instructions` section on session snapshot
- `CognitiveOutcome` → Rule elevation pathway via hotel IPC

---

## Open Questions

- Should `active_plan` survive across approval interrupts? (Yes — checkpoint includes it)
- Should the model be able to update individual plan steps, or only produce a new plan? (New plan on re-plan, step status updated by philote from tool results)
- Should `memory.recall` accept a `scope` parameter for cross-scope queries? (Deferred — `SelfOnly` default for now)
- Should stall detection threshold be per-role configurable? (Yes — part of `ExecutionPolicy`)
