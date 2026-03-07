# Philotic Personality and Context Proposal

## Goal

Define how Philotic should construct a recognizable agent personality and a useful context window without collapsing identity, user adaptation, and memory into one giant prompt blob.

This proposal focuses on:

- durable personality bootstrapping
- OpenClaw/ZeroClaw compatibility for existing agents
- per-user continuity
- memory and context layering
- making the agent feel like someone, not just a routing success story

## Disposition

Proposed and pinned for the next personality-focused slice.

Research basis:

- ZeroClaw/OpenClaw bootstrap personality from workspace files like `SOUL.md`, `IDENTITY.md`, `USER.md`, and `MEMORY.md`
- ZeroClaw also keeps a separate dynamic memory-loader seam for retrieved context instead of dumping all memory into the bootstrap prompt

Track follow-on work in [task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Core Recommendation

Split Philotic prompt/context assembly into five intentional layers:

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

If those layers are merged carelessly, personality becomes generic, memory becomes noisy, and the session prompt becomes a sentimental landfill.

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
