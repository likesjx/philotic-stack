---
name: muninn-memory-protocol
description: Use this skill when a client should adopt the shared Muninn memory workflow for continuity across sessions. Trigger when setting up Muninn-backed recall/write-back habits, wiring a client to the shared helper, or standardizing memory behavior across multiple cognitive clients.
---

# Muninn Memory Protocol

Use this skill when you need a client to participate in the shared Muninn memory experiment.

Read [MUNINN_CLIENT_MEMORY_PROTOCOL.md](../../docs/reference/MUNINN_CLIENT_MEMORY_PROTOCOL.md) first for the portable contract.

## Purpose

This skill standardizes how a client should:

- retrieve before meaningful work
- write back after durable outcomes
- keep memory atomic and fragmented into short, meaningful bursts
- rely on the shared helper instead of hand-rolling MCP ceremony

## Use The Shared Helper

Preferred transport helper:

- [muninn_mcp.py](../../scripts/muninn_mcp.py)

Do not reimplement the MCP handshake ad hoc unless you are intentionally porting the helper into another runtime.

## Default Habit

Before meaningful work:

- run `bootstrap`
- run `where-left-off`
- run `recall`

After durable outcomes:

- run `remember` for atomic facts, preferences, and outcomes in short, meaningful bursts
- run `decide` for explicit decisions with rationale

Keep writes small:

- `remember`: 1-3 sentences, preferably under ~300 chars, hard ceiling ~500
- `decide`: concise rationale, preferably under ~500 chars, hard ceiling ~800

Use a small tag vocabulary only when it helps retrieval:

- `flesh-out`
- `decision`
- `reality-gap`
- `validation`
- `follow-up`
- `operator-preference`

## Failure Rule

The helper must be allowed to block.

If `python3 scripts/muninn_mcp.py bootstrap` fails:

- alert the user/operator immediately
- do not pretend retrieval already happened
- do not continue with meaningful work until explicit approval is given to proceed without Muninn

The protocol is continuity first, not silent degradation dressed up as professionalism.

## Truth Rule

Recalled memory is guidance, not authority.

If recalled memory conflicts with current observed repo/runtime truth:

- trust observed truth
- note the mismatch if it matters

## Why The Helper Is Shared Instead Of Skill-Only

This skill is a client adapter.

The helper is transport infrastructure.

If the helper exists only inside a client-specific skill:

- other cognitive clients cannot reuse it cleanly
- the protocol becomes trapped in one client packaging format
- every new client has to rediscover the same handshake logic

So the canonical helper should live in normal versioned project infrastructure, and client skills should wrap it.
