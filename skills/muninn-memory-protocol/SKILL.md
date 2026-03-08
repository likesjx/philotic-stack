---
name: muninn-memory-protocol
description: Use this skill when a client should adopt the shared Muninn memory workflow for continuity across sessions. Trigger when setting up Muninn-backed recall/write-back habits, wiring a client to the shared helper, or standardizing memory behavior across multiple cognitive clients.
---

# Muninn Memory Protocol

Use this skill when you need a client to participate in the shared Muninn memory experiment.

Read [MUNINN_CLIENT_MEMORY_PROTOCOL.md](/Users/jaredlikes/code/philotic-stack/docs/reference/MUNINN_CLIENT_MEMORY_PROTOCOL.md) first for the portable contract.

## Purpose

This skill standardizes how a client should:

- retrieve before meaningful work
- write back after durable outcomes
- keep memory atomic
- rely on the shared helper instead of hand-rolling MCP ceremony

## Use The Shared Helper

Preferred transport helper:

- [muninn_mcp.py](/Users/jaredlikes/code/philotic-stack/scripts/muninn_mcp.py)

Do not reimplement the MCP handshake ad hoc unless you are intentionally porting the helper into another runtime.

## Default Habit

Before meaningful work:

- run `where-left-off`
- run `recall`

After durable outcomes:

- run `remember` for atomic facts, preferences, and outcomes
- run `decide` for explicit decisions with rationale

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
