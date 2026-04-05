---
title: Memory Layering and Work Product Split Proposal
doc_type: proposal
domain: memory-context
status: proposed
last_updated: 2026-04-04
tags:
- memory
- context
- datasource
- work-product
- references
related_docs:
- PERSONALITY_AND_CONTEXT_PROPOSAL.md
- GRAPH_DATASOURCE_PROPOSAL.md
- MUNINN_MEMORY_PROTOCOL_PROPOSAL.md
- SESSION_LOOP_PROPOSAL.md
- ARCHITECTURE_STATUS.md
task_refs:
- docs/task.md
proposal_id: memory-layering-and-work-product-split
implements: []
implemented_by: []
active_seams:
- structured-context-layers
- graph-datasource
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Memory Layering and Work Product Split Proposal

## Goal

Define a stronger layering model for Philotic memory and context so the system can distinguish:

- what is live in the current role turn
- what is heuristically recalled
- what should remain as durable structured reference
- what is actual work-product or life-product data

This proposal also clarifies how those layers relate to the agent/role identity stack:

- agent: soul, identity, rules
- role: addendum, manifest, skills, tools

The point is not to create four poetic synonyms for "memory." The point is to stop forcing one layer to impersonate working state, learned continuity, durable references, and polished artifacts all at once.

## Disposition

Proposed.

Current Philotic runtime already has partial layering:

- working turn state lives in the session turn window
- heuristic recall can come from Muninn or similar memory engines
- base identity and role overlays are projected separately in `philote`
- graph-backed artifacts and broader data seams are emerging through `graph-datasource`

What is still missing is a canonical split between:

- heuristic memory
- durable rote/reference memory
- graph-backed work product

## Current Slice

Park the conceptual boundary now so future implementation does not collapse these concerns into one "memory" bucket again.

This slice should:

- name the four memory layers explicitly
- distinguish agent memorization from datasource truth
- define overlap rules between graph-datasource, agent memorization, and skills
- clarify how rules, references, and work product differ

This slice does **not** attempt to implement storage, projection, or UI for all four layers.

## Core Recommendation

Philotic should use four distinct memory/context layers:

1. working memory
2. heuristic memory
3. rote memory
4. work product

Those layers should feed context projection differently and should not share ownership just because they all involve "things the agent knows."

## The Four Layers

### 1. Working Memory

**Owner:** current role turn window

**Examples:**

- active task framing
- current plan state
- tool-call history
- active conversational thread
- in-flight approvals

**Characteristics:**

- role-local
- ephemeral
- refreshed constantly
- should roll over with turn/session lifecycle

This is the live desk surface, not the filing cabinet.

### 2. Heuristic Memory

**Owner:** memory engine / recall layer

**Examples:**

- user collaboration preferences
- prior decisions that matter again
- patterns about what usually works
- relationship fit and continuity

**Characteristics:**

- relevance-ranked
- probabilistic / advisory
- recall-oriented
- may come from Muninn or a future Philotic-native memory layer

This layer answers: "What should probably come back into view right now?"

### 3. Rote Memory

**Owner:** graph-backed memorization/reference layer

**Examples:**

- pointers
- dates
- names
- standing references
- structured reminders
- "keep this on hand" notes
- idea fragments that should stay discoverable

**Characteristics:**

- durable
- structured or semi-structured
- discoverable without fuzzy recall roulette
- can be shared at agent scope or role scope
- sits above heuristic memory and below full work-product storage

This layer answers: "What should remain easy to find because it matters often enough to keep nearby?"

Rote memory is not the same thing as rules:

- rules are normative
- rote memory is declarative

### 4. Work Product

**Owner:** datasource / artifact layer

**Examples:**

- calendar records
- goals
- project artifacts
- music practice data
- datasets
- polished notes
- shareable planning outputs
- durable system-wide records

**Characteristics:**

- source-of-truth data
- shareable and inspectable
- often system-wide or domain-wide
- not just "what the agent remembers"

This layer answers: "What is the actual data or artifact?"

## Datasource vs. Memorization

The `graph-datasource` concept should own work-product truth, not all memory-like things.

Recommended split:

- `graph-datasource` stores the actual data
- agent memorization stores learned or curated references to that data
- skills provide structured access and transformation paths across both

Example:

- datasource:
  - piano repertoire
  - practice history
  - calendar events
  - goals
- memorization:
  - this repertoire matters right now
  - surface progress against this goal
  - this date is important
  - this pointer should stay close at hand
- skill:
  - fetches, interprets, updates, or summarizes both

Intentional overlap is acceptable if ownership remains explicit:

- datasource owns the artifact
- memorization owns the agent's learned relationship to the artifact
- skills own procedures, not truth

## Agent and Role Stack

The memory layering model should remain separate from the identity/posture stack.

### Agent Layer

- soul
- identity
- rules

Agent-level rules are durable normative constraints or standing behavioral guidance.

### Role Layer

- addendum
- manifest
- skills
- tools

Role posture is additive over one continuous self. A role is not a second agent wearing the same storage like a trench coat.

Recommended interpretation:

- `role_identity_addendum` = role-specific posture overlay
- `role_manifest` = governance / operating contract
- skills and tools = afforded execution surface for that role

## Rules vs. References

Philotic should distinguish clearly between:

### Rules

What the agent or role should do.

Examples:

- do not bypass approval
- keep replies compact on Telegram
- push back honestly when it improves the work

### References

What the agent or role should keep on hand.

Examples:

- important dates
- stable pointers
- key people or projects
- standing notes
- durable reminders

Rules are behavioral.
References are factual.

Both should be durable and discoverable, but they are not the same thing.

## Projection Model

Turn-time context assembly should eventually be able to pull from all four layers:

- working memory for what is happening now
- heuristic memory for what matters by relevance
- rote memory for what should stay close at hand
- work product for authoritative factual artifacts

Not every agent or role needs the same projection profile.

Examples:

- conversational persona:
  heavier heuristic + rote recall, lighter work-product detail
- specialist role:
  heavier role references and relevant work-product pointers
- subagent worker:
  mostly working memory + narrowly scoped references

## Overlap Rules

Use this ownership heuristic:

- if the item is a polished artifact or durable factual record, it belongs in work product
- if the item is a learned continuity cue, it belongs in heuristic memory
- if the item is a durable pointer or standing fact the agent should keep near, it belongs in rote memory
- if the item only matters for the current role turn, it belongs in working memory

When an item appears in multiple layers, each copy must serve a different purpose:

- datasource copy = authoritative record
- memorization copy = salience / relationship / projection cue
- working copy = immediate operational use

## Why This Boundary Matters

Without this split:

- turn windows become sentimental landfills
- heuristic memory gets asked to act like a project database
- work product gets buried in recall systems
- rules and references blur into one vague bag of "stuff to remember"

That is a very efficient way to make retrieval, projection, and operator trust all worse at the same time.

## Follow-On Seams

1. Define graph schema for rote memory / durable references at agent and role scope.
2. Define projection policy for when rote references enter the context window.
3. Align desktop/editor surfaces so rules, references, and work-product pointers are edited separately.
4. Revisit `GRAPH_DATASOURCE_PROPOSAL.md` so its scope is explicitly work-product and datasource truth, not generic memorization.
5. Decide whether role-scoped durable references live directly on role records or as adjacent graph entities.
