# Multi-Hotel Component Distribution Proposal

## Goal

Define how Philotic should support splitting one end-to-end user interaction across multiple hotels, such as membrane on one hotel, agent on another, model on another, and tool runner on another.

## Disposition

`proposed`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Core Recommendation

Philotic should support **distributed component placement** as a first-class runtime pattern, not just as an accidental consequence of remote model/tool routing.

The intended shape is:

- membrane may live on one hotel
- agent may live on another
- model capability may resolve to another
- tool runner capability may resolve to another

while the route contract, session ownership, and reply path remain coherent.

## Why This Matters

The “three-body problem” is really a four-body problem with attitude:

- external interface hotel
- cognition hotel
- model hotel
- tool/execution hotel

If Philotic can only handle remote model fallback, then it has not yet proven the architecture it is gesturing toward.

## Non-Negotiable Invariants

### 1. Session-owned membrane reply routing

The reply path must remain owned by the session’s membrane binding.

Remote placement must never opportunistically choose a different membrane just because it looks convenient.

### 2. Shared routing vocabulary

All distributed hops should keep using the same route contract:

- `target_node`
- `target_role`
- optional pinned `incarnation_id`
- optional placement hints

### 3. Structured turn ownership

The active turn must remain coherent even when execution fans out across hotels.

### 4. Durable hop boundaries

Each routed hop should have durable request/result correlation and auditable failure handling.

## Current Reality

Today, Philotic has proven:

- remote model routing for `text.generate` and `media.analyze`
- first remote tool fallback placement
- TCP execution plane for routed inter-hotel task traffic

But a broader multi-hotel vertical slice is still open because:

- membrane is intentionally session-owned and not yet part of general remote placement
- broader routed component classes are not all using the same remote-capable path yet
- inter-hotel ACK truth is still transitional
- trust/perimeter policy is not closed enough for a serious cross-host split

## Recommended Validation Ladder

### Stage 1

Local multi-hotel:

- membrane on hotel A
- agent on hotel B
- model on hotel C

### Stage 2

Local four-hotel:

- membrane on hotel A
- agent on hotel B
- model on hotel C
- tool runner on hotel D

### Stage 3

Cross-host:

- at least one hop off the local machine
- explicit perimeter/trust policy enabled

## First Slice Recommendation

Before attempting the full distributed split:

1. extend remote-capable route metadata across remaining routed component classes
2. move ACK behavior toward strict post-commit truth
3. finish the perimeter/trust model for cross-host joins
4. define the first watched multi-hotel validation script for the membrane/agent/model/tool split
