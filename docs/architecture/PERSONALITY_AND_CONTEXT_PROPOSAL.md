---
title: "Philotic Personality and Context Proposal"
doc_type: proposal
domain: memory-context
status: accepted-current-slice
last_updated: 2026-03-12
tags:
  - personality
  - context
  - identity
  - projection
  - active-seam
related_docs:
  - ARCHITECTURE_STATUS.md
  - PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md
  - ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: personality-and-context
implements: []
implemented_by:
  - turn-time-projection-slice
  - imported-jane-profile-slice
active_seams:
  - structured-context-layers
  - legacy-workspace-import
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
---

# Philotic Personality and Context Proposal

## Goal

Define how Philotic should construct a recognizable agent personality and a useful context window without collapsing identity, user adaptation, and memory into one giant prompt blob.

This proposal focuses on:

- durable personality bootstrapping
- OpenClaw/ZeroClaw compatibility for existing agents
- per-user continuity
- memory and context layering
- turn-time projection functions instead of static prompt blobs
- different projection profiles for conversational agents, workers, and subagents
- making the agent feel like someone, not just a routing success story

## Disposition

Accepted for the current slice. Initial turn-time projection scaffolding is implemented in `agent-core`, and the canonical session snapshot now carries a graph-backed imported Jane profile seeded from the legacy `vps-jane` workspace. The runtime is still using compatibility text inputs rather than richer heuristic memory projection.

Research basis:

- ZeroClaw/OpenClaw bootstrap personality from workspace files like `SOUL.md`, `IDENTITY.md`, `USER.md`, and `MEMORY.md`
- ZeroClaw also keeps a separate dynamic memory-loader seam for retrieved context instead of dumping all memory into the bootstrap prompt

Track follow-on work in [task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Current Slice

Land the first honest projection boundary in `agent-core` and feed it with one imported Jane profile:

- refactor prompt assembly around:
  - `project_agent_self`
  - `project_user`
  - `project_knowledge`
- seed `agent-jane-01` from the canonical `~/.openClaw/workspace` bootstrap files
- expose that imported profile in the canonical session snapshot
- keep the first implementation compatibility-first and text-driven
- preserve existing runtime behavior while creating a clean seam for later:
  - user-specific profile projection
  - Muninn or other memory-backed knowledge projection
  - fuller `openclaw.json` import

## Core Recommendation

Philotic should evaluate personality, user, and memory as turn-time projection functions.

The minimal triad is:

1. who am I
2. who am I talking to
3. what do I know that matters right now

In implementation terms, that becomes:

- `project_agent_self(turn_context)`
- `project_user(turn_context)`
- `project_knowledge(turn_context)`

Those projections should then be composed with current session/runtime state.

For now, the simplest implementation can still be fed by imported or authored text anchors. But the abstraction must be dynamic from day one.

The same principle should apply to skills and tools:

- full capability inventory is runtime truth
- turn-local capability exposure should be projection-driven
- if the current goal does not make a tool or skill relevant, it should usually stay out of the prompt/context window

That keeps token usage lower and reduces the model’s temptation to fondle irrelevant machinery.

This proposal now aligns with the model-controller request envelope split:

- personality/context projections should fill the structured `context` object
- skills and tools should be projected into a separate `affordances` object
- the model-controller should receive structured layers, not one flattened prompt blob

That separation matters because semantic context and executable affordances optimize differently and should not be forced into the same trimming path.

## Projection Layers

The practical projection layers for the first implementation should be:

1. soul
2. identity
3. user context
4. memory context
5. session context

That should replace the current flatter prompt assembly path in `agent-core`.

The key rule is:

- soul and identity define who the agent is
- user context defines who the agent is talking to
- memory context defines what is important to recall now
- session context defines what is happening right now

Role-incarnation implication:

- role posture is layered on top of identity; it is not a substitute for identity
- role addenda should be projected after base identity/user layers and before current task/session context
- context management must preserve one canonical agent identity layer even when multiple role incarnations are active

If those layers are merged carelessly, personality becomes generic, memory becomes noisy, and the session prompt becomes a sentimental landfill.

## Source, Model, Projection

Philotic should explicitly distinguish:

### 1. Source

Imported or authored input.

Examples:

- `SOUL.md`
- `IDENTITY.md`
- `USER.md`
- `MEMORY.md`
- Philotic-authored records

### 2. Model

The internal representation.

Initially this can be very light, but it should be separate from raw source text.

### 3. Projection

The evaluated turn-time output used for the current context window.

This distinction matters because legacy files should be input only, not the final runtime ontology.

## Legacy Reference

### What ZeroClaw/OpenClaw do

The legacy bootstrap path loads a fixed set of workspace files into the system prompt:

- `AGENTS.md`
- `SOUL.md`
- `TOOLS.md`
- `IDENTITY.md`
- `USER.md`
- optional `BOOTSTRAP.md`
- `MEMORY.md`

Then a separate memory-loader path can inject dynamically recalled context for the current turn.

That separation is worth preserving:

- static personality and relationship files
- curated durable memory
- dynamic retrieval

For Philotic, these legacy files should not all be projected equally into ordinary conversation.

- `SOUL.md`, `IDENTITY.md`, and `USER.md` are valid source inputs for turn-time projection
- `MEMORY.md` is a seed/retrieval source, not a wholesale prompt dump
- `AGENTS.md` should be treated as operational/bootstrap guidance, not default conversational personality context

If `AGENTS.md` is projected directly into ordinary chat turns, operational instructions and shell habits bleed into the agent voice, which is informative in exactly the wrong way.

## Proposed Personality Model

### 1. Soul

`soul` is the deepest durable personality layer.

It should capture:

- values
- emotional tone range
- style defaults
- relational instincts
- what makes the agent feel like itself

This is the closest analogue to `SOUL.md`.

It should be durable and recognizable across sessions, hotels, and incarnations.

### 2. Identity

`identity` is the explicit self-model.

It should capture:

- name
- role
- self-description
- operator relationship framing
- visual/representation details later if needed

This is the closest analogue to `IDENTITY.md`.

Identity is agent-level canonical state, not role-local state. Role incarnations may add specialized stance text, but that text should not replace the underlying self-model.

Identity is not the same thing as soul:

- soul = inner behavioral continuity
- identity = outward self-definition

### 3. User Context

`user context` should follow the person, not the session.

This is the analogue to `USER.md`, but Philotic should make it more structured.

It should capture:

- who the user is
- stable preferences
- relationship context
- interaction preferences
- learned collaboration patterns

Important recommendation:

- keep a durable user profile per identified user
- allow the agent to learn how to work best with that user over time
- do not make this purely static author-written text

So `USER.md` should evolve into:

- seeded user profile
- learned interaction notes
- memory-backed collaboration preferences

This is where the “how you work with me” layer belongs.

### 4. Memory Context

Memory should not be one thing.

Philotic will likely need multiple memory substrates:

- context graph
- knowledge graph
- hippocampal / episodic layer
- heuristic/working memory layers
- possibly index-local memory inside tool-runners

So prompt assembly should not assume one backend or one retrieval method.

Instead, memory context should be assembled from:

- curated durable memory summary
- retrieved relevant episodic/contextual notes
- task-relevant external/tool-local references when needed

Recommendation:

- treat memory as a retrieval-and-ranking problem, not a giant markdown dump
- only inject what is important right now
- allow tool-runners to maintain their own specialist indexes, but surface those through retrieval rather than prompt sprawl

### 5. Session Context

This is the current-turn operational layer.

It should include:

- session objective
- recent turns
- compacted summary
- tool/session bindings
- workspace
- approval policy
- current active turn state

This is the layer most similar to the current `SessionState::build_prompt()` behavior, but it should sit under the deeper personality layers.

## Agent Modes

Philotic should not assume every agent projection is socially identical.

The same underlying substrate should support at least three broad projection profiles:

### 1. Conversational Agent

This is the agent you talk with and build continuity with.

Projection emphasis:

- strong selfhood
- relationship continuity
- adaptive style
- richer personality expression

### 2. Worker

This is the agent you expect precision and dependable output from.

Projection emphasis:

- clarity
- discipline
- narrower personality expression
- high legibility

### 3. Subagent

This is the bounded agent you want to complete a job.

Projection emphasis:

- task scope
- low social overhead
- minimal relationship projection
- bounded context

These should be treated as projection profiles, not entirely different species of mind.

## OpenClaw Compatibility

Philotic should support importing existing agents from `openclaw.json` and related workspace files.

Near-term compatibility targets:

- import agent definitions from `openclaw.json`
- ingest:
  - `SOUL.md`
  - `IDENTITY.md`
  - `USER.md`
  - `MEMORY.md`

Recommendation:

- import them as initial Philotic records, not merely raw prompt text
- preserve the original markdown as source artifacts where useful
- normalize them into structured Philotic concepts over time

This is important because you already have agents with real identity continuity there, and rebuilding them by hand would be unnecessary suffering dressed up as purity.

## User Continuity

Philotic should support multi-user identity eventually.

That means:

- a conversation should resolve to a user identity when possible
- user context should follow that person across sessions
- the agent should be able to adapt interaction style per user

Recommendations:

- user context belongs to the identified user, not the agent session
- sessions may include a projection of user context
- learned interaction preferences should come from memory and observed successful collaboration patterns

This creates a better model:

- static user profile
- learned collaboration profile
- session-local adaptation

## Memory Architecture Guidance

Memory is the part most likely to become overcomplicated, so Philotic should define memory roles before choosing one magical backend.

Recommended roles:

- durable profile memory
  - soul/identity/user continuity

- episodic memory
  - notable past interactions and decisions

- semantic/knowledge memory
  - facts, structured references, domain knowledge

- working memory
  - current turn/session state and compacted short-term context

- specialist/tool-local indexes
  - domain-specific retrieval inside runners when useful

The assembly rule should be:

- inject only what is relevant to the moment
- prefer ranked retrieval and summaries over raw accumulation

## Heuristic Direction

Long-term, Philotic should move toward a heuristic mind model rather than a fixed schema of personality fields.

That likely means:

- stable anchors
- memory-backed user fit
- salience-driven knowledge projection
- later:
  - goals
  - fears
  - needs and wants
  - relationships
  - associations and jumps of logic

But the first implementation should stay small and observable.

## Personality Guidance

Personality should be recognizable, not theatrical.

Recommendations:

- personality may include emotional range, but should not become mood cosplay
- the user should be able to tell which agent they are talking to
- continuity should come from soul + identity + learned user interaction, not just catchphrases

The design target is:

- distinct presence
- coherent values
- adaptive relationship style
- not generic assistant voice number 47

## Implementation Recommendation

Near-term implementation order:

1. define explicit prompt assembly layers in `agent-core`
2. add support for:
   - `soul_text`
   - `identity_text`
   - `user_context_text`
   - `memory_summary`
3. keep current session context assembly, but move it into the lowest layer
4. add import scaffolding for OpenClaw workspace files and `openclaw.json`
5. later split text fields into more structured graph entities

## Immediate Next Slice

The first implementation slice should do three things:

1. refactor prompt assembly to use explicit personality/context layers
2. seed agent-level soul/identity/user text inputs
3. define the import path for legacy OpenClaw agent files

That gives Philotic a real personality scaffold before we dive into the full memory maze.
