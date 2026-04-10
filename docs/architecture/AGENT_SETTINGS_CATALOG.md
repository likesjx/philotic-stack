---
title: Agent Settings Catalog
doc_type: reference
domain: runtime-sessions
status: active
last_updated: 2026-03-31
tags:
- settings
- agent-loop
- context-envelope
- memory
- execution
related_docs:
- COGNITIVE_LOOP_PROPOSAL.md
- PHILOTIC_AGENT_LOOP_SPEC.md
task_refs:
- docs/task.md
---

# Agent Settings Catalog

Settings for a philote session are grouped into three policy structs under `AgentSettings`.
They live in the context graph keyed by `agent_id`, are fetched at session init, and are
configurable at runtime via `agent.configure` using the `settings.*` config path prefix.

---

## `settings.context_window.*`

Controls how the rolling `dialogue_window` context section is assembled and bounded.

| Path | Type | Default | Min | Max | Description |
|---|---|---|---|---|---|
| `settings.context_window.dialogue_window_minutes` | `u32` | `10` | `2` | `60` | Maximum age of turns included in the window (minutes). Turns older than this are dropped. Time-based filtering requires turn timestamps — currently the char budget is the primary bound. |
| `settings.context_window.dialogue_window_chars` | `usize` | `10_000` | `1_000` | `50_000` | Maximum total character budget for the dialogue window. Oldest turns are dropped first when the budget is exceeded. |
| `settings.context_window.include_tool_calls` | `bool` | `true` | — | — | When true, assistant turns in the dialogue window include tool call names and args alongside the response text. |

---

## `settings.memory.*`

Controls the two-axis memory strategy: local rolling window + on-demand Muninn recall.

| Path | Type | Default | Min | Max | Description |
|---|---|---|---|---|---|
| `settings.memory.memory_window_size` | `usize` | `10` | `3` | `30` | Number of recent turns kept in `recent_turns`. Older turns roll off. This is the recency axis — what's always in context without a recall call. |
| `settings.memory.long_term_recall_enabled` | `bool` | `true` | — | — | When true, the `memory.recall` local agent tool is available. When false, the model cannot request on-demand Muninn retrieval for this session. |
| `settings.memory.recall_limit` | `usize` | `5` | `1` | `20` | Default result limit passed to `engine.activate()` when the model calls `memory.recall` without an explicit limit argument. |

---

## `settings.execution.*`

Controls the cognitive execution loop: iteration cap, plan behaviour, and stall detection.

| Path | Type | Default | Min | Max | Description |
|---|---|---|---|---|---|
| `settings.execution.iteration_cap` | `u32` | `10` | `1` | `50` | Maximum model round-trips per turn. Exhausting the cap fails the turn with a clear error rather than looping indefinitely. Replaces the old hardcoded `MAX_TOOL_ITERATIONS` constant. |
| `settings.execution.plan_required_on_skill` | `bool` | `true` | — | — | When true, a structured `active_plan` is required as the first model output whenever a skill is activated. |
| `settings.execution.stream_tool_events` | `bool` | `true` | — | — | When true, intermediate turn events (`step_started`, `step_completed`, `step_failed`) are emitted to membrane during execution. |
| `settings.execution.stall_detection_threshold` | `u32` | `3` | `1` | `10` | Number of consecutive step failures before the loop surfaces to the user instead of continuing. The "Ralph Wiggum" guard. |

---

## Configuring at Runtime

Use `agent.configure` with `operation: "set"`:

```
agent.configure
  config_path: "settings.execution.iteration_cap"
  value: 20
  operation: "set"
```

All values are clamped to their valid range on write. The model can configure its own
settings via `agent.configure` — the `"config"` tool class requires operator approval.

---

## Storage

Settings are stored in the context graph, keyed by `agent_id`. Fetched at session init
alongside the agent profile. Not yet wired to hotel IPC (all sessions use `Default` until
per-agent storage is implemented in a future slice).
