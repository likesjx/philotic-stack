# Agent Plugin Hooks Proposal

## Goal

Define the plugin/hook boundaries `agent-core` should expose so new context engines, memory engines, local models, and control-plane behaviors can be integrated without repeatedly cutting into the main turn loop.

## Disposition

`proposed`

Track related work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Core Recommendation

Treat `agent-core` as a host for bounded extension hooks, not as the permanent owner of every new runtime concern.

The first hook families should cover:

- context assembly
- memory lookup/store
- media transforms such as transcription
- model capability routing hints
- admin/control intercepts

## Why This Matters

Philotic is already plugin-shaped at the system level, but `agent-core` still absorbs too much behavior directly.

Without hooks:

- every new subsystem becomes a loop edit
- testing gets harder
- “plugin architecture” stays true everywhere except the place that hurts most

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

## First Slice Recommendation

Define the first hook registry and contract shapes, then move one current seam behind it:

- transcription
- or memory lookup

That proves the extension model before it turns into an abstraction festival.
