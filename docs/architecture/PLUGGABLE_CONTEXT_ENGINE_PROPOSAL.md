# Pluggable Context Engine Proposal

## Goal

Define a clean boundary for how Philotic assembles context for an agent turn so context sources, retrieval strategies, and ranking policies can evolve without turning `agent-core` into one giant opinionated glue pile.

## Disposition

`proposed`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Core Recommendation

Introduce a **context engine** contract that is owned by the hotel/runtime boundary, not by one model provider or one agent implementation.

That engine should:

- collect candidate context from canonical sources
- rank, filter, and budget that context deterministically
- expose a stable turn-ready context payload to `agent-core`
- allow multiple implementations behind one contract

## Why This Needs To Exist

Philotic is accumulating multiple context sources:

- session snapshot
- imported agent identity bundles
- graph-backed profile/config
- memory systems
- tool and capability state
- future external retrieval engines

If `agent-core` owns all of that assembly directly, it becomes impossible to change context strategy without changing the cognitive loop itself.

## Recommended Boundary

The context engine should own:

- source selection
- ordering and ranking
- token/size budgeting
- deterministic inclusion/exclusion rules
- provenance metadata for debugging

`agent-core` should consume:

- a bounded, ordered context payload
- not a pile of half-ranked raw records

## First Implementations To Support

1. graph-native context assembly
2. imported OpenClaw/ZeroClaw identity projection
3. memory-backed augmentation
4. emergency/local fallback context mode

## First Slice Recommendation

Define the first trait or request/response contract for:

- `build_context(agent_id, session_id, turn_id, mode)`

and make the current inline assembly path one implementation of that contract before adding more engines.
