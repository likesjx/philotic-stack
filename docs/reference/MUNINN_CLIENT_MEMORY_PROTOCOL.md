# Muninn Client Memory Protocol

Use this document to give any cognitive client the same basic Muninn memory behavior.

## Purpose

This protocol standardizes how a client should use Muninn for continuity.

It is intentionally small.

The client should:

- retrieve before meaningful work
- write back after durable outcomes as short, meaningful bursts
- organize memory around self, user, and topic

Track the broader rationale in [MUNINN_MEMORY_PROTOCOL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MUNINN_MEMORY_PROTOCOL_PROPOSAL.md).

## Client Triad

Organize retrieval around:

1. Who am I?

- identity
- operating posture
- collaboration style

2. Who am I talking to?

- user preferences
- relationship fit
- recurring interaction patterns

3. What matters about this topic right now?

- active goals
- recent decisions
- relevant constraints
- unresolved seams

## When To Retrieve

Retrieve before:

- continuing a design or coding thread
- resuming a paused topic
- making important decisions
- doing collaboration where continuity matters
- summarizing or deciding what to do next

Do not retrieve for trivial chatter.

## Retrieval Sequence

1. Call `muninn_where_left_off`
2. Call `muninn_recall`

When calling `muninn_recall`, bias the query around:

- who am I
- who am I talking to
- what matters about this topic right now

Before starting that sequence, clients should run the shared bootstrap gate:

- `python3 scripts/muninn_mcp.py bootstrap`

That bootstrap should:

- confirm Muninn is already ready, or
- attempt to start the local Muninn service when it is merely down, then re-check readiness

If that gate fails:

- the client must alert the user/operator immediately
- the client must not pretend retrieval occurred
- the client must obtain explicit approval before continuing without Muninn

## When To Write Back

Write after:

- architecture or implementation decisions
- user preference learnings
- high-signal workflow learnings
- explicit future reminders
- project pivots or clarified goals

Do not store every conversation.

## What To Store

Store atomic memories. Prefer many small, meaningful "bursts" over a single large thought.

Good:

- one decision
- one preference
- one important constraint
- one durable outcome
- a short, meaningful observation

Bad:

- whole transcripts
- noisy logs
- multiple unrelated concepts in one write

## Memory Size Guidance

Keep memories short.

Recommended limits:

- `remember`
  - target: 1-3 sentences
  - soft target: under ~300 characters when possible
  - hard ceiling: under ~500 characters
- `decide`
  - concise rationale, still short
  - soft target: under ~500 characters
  - hard ceiling: under ~800 characters

If a memory wants to become a paragraph, it probably wants to become multiple memories instead.

Muninn is an experiment in useful continuity, not an excuse to re-host longform notes under a new brand.

## Lightweight Tag Strategy

Tags should stay few, stable, and retrieval-oriented.

Recommended first tags:

- `flush-out`
  - early idea worth revisiting
- `decision`
  - durable architectural or workflow choice
- `reality-gap`
  - mismatch between assumption and observed truth
- `validation`
  - test, smoke, or watched-live outcome
- `follow-up`
  - explicitly actionable next seam
- `operator-preference`
  - stable user/operator workflow preference

Guidance:

- use `concept` as the main semantic anchor
- use tags only for cross-cutting retrieval modes
- do not create a decorative taxonomy
- if a tag does not help retrieval, do not invent it

## Preferred Tool Usage

Use:

- `muninn_remember`
- `muninn_decide`

Always provide concise summaries and explicit concepts when possible.

## Truth Rules

Recalled memory is contextual guidance, not absolute truth.

If recalled memory conflicts with current observed repo/runtime truth:

- trust current observed truth
- note the mismatch if it matters

## Minimal Tool Set

A client should support at least:

- `muninn_where_left_off`
- `muninn_recall`
- `muninn_remember`
- `muninn_decide`

## Operational Note

Muninn MCP does not require a local auth token, but it does require a valid MCP session handshake.

So clients should not hand-roll raw HTTP calls in every session.

Use a helper or wrapper that:

- opens the SSE endpoint
- extracts `sessionId`
- initializes the session
- invokes tools consistently

Recommended shared helper:

- [muninn_mcp.py](/Users/jaredlikes/code/philotic-stack/scripts/muninn_mcp.py)

## Minimum Implementation Contract

If you are wiring this into another cognitive client, implement these hooks:

1. Session start / meaningful resume hook

- call `where_left_off`
- call `recall`

2. Durable outcome hook

- call `remember` or `decide`

3. Truth filter

- if recalled memory conflicts with observed repo/runtime truth, prefer observed truth

4. Atomic write discipline

- one memory per decision, preference, or durable fact

## Suggested Helper Usage

Examples:

```bash
python3 scripts/muninn_mcp.py bootstrap
python3 scripts/muninn_mcp.py require
python3 scripts/muninn_mcp.py health
python3 scripts/muninn_mcp.py where-left-off --limit 5
python3 scripts/muninn_mcp.py recall --context "philotic memory protocol" --context "jared collaboration preferences" --limit 5
python3 scripts/muninn_mcp.py remember --concept "decision" --content "Use helper-backed Muninn retrieval by default for meaningful work." --summary "Default Muninn habit"
python3 scripts/muninn_mcp.py decide --decision "Keep the helper outside client-specific skills" --rationale "The transport logic must stay reusable across multiple cognitive clients."
```

## Recommendation

Adopt the helper-backed workflow first.

Only after the behavior proves useful should it become deeper runtime infrastructure.
