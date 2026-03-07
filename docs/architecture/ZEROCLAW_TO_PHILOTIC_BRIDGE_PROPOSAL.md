# ZeroClaw to Philotic Bridge Proposal

## Goal

Bring existing ZeroClaw/OpenClaw agents into the Philotic web as real, materializable agents before attempting a full next-generation redesign of personality, memory, and context.

This proposal is intentionally pragmatic:

- preserve what already works
- import the agents you already have
- materialize them in Philotic
- evolve them in place

That is much better than waiting for the perfect theory of synthetic personhood while your existing agents remain trapped in the old house.

## Disposition

Proposed and pinned as the recommended next bridge project.

This project should follow the current personality/context work and provide the migration path from legacy ZeroClaw/OpenClaw agent definitions into Philotic-native session and runtime architecture.

Track follow-on work in [task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Core Recommendation

Do not attempt a full legacy rewrite first.

Instead:

1. import legacy agents
2. materialize them in Philotic with compatibility inputs
3. preserve their recognizable personality and context
4. gradually replace the legacy prompt/bootstrap assumptions with Philotic-native modeling

So the bridge should be:

- compatibility-first
- evolution-friendly
- not a one-shot migration fantasy

## What Should Be Imported

### Required legacy inputs

From `openclaw.json` and the associated workspace:

- agent definitions
- `SOUL.md`
- `IDENTITY.md`
- `USER.md`
- `MEMORY.md`

Potential later imports:

- `TOOLS.md`
- `BOOTSTRAP.md`
- agent-specific config and model preferences
- skill references

These should be treated as source input, not as the final runtime ontology.

## Bridge Model

The bridge should distinguish three layers:

### 1. Legacy Source

The original OpenClaw/ZeroClaw materials.

Examples:

- `openclaw.json`
- `SOUL.md`
- `IDENTITY.md`
- `USER.md`
- `MEMORY.md`

These remain useful because they encode real identity continuity and authored intent.

### 2. Philotic Compatibility Record

A Philotic-side imported agent record that stores:

- source references
- imported text artifacts
- initial normalized fields
- import metadata/versioning

Examples:

- `source_system = "openclaw"`
- `source_agent_id`
- `soul_text`
- `identity_text`
- `user_context_text`
- `memory_summary`
- `imported_at`

This gives Philotic a stable imported form without pretending it already understands everything structurally.

### 3. Philotic Projection

What the running Philotic agent actually uses:

- prompt/context layers
- session bindings
- runtime policies
- tool visibility
- user/session projections

This is the live execution form and can evolve independently of the original source files.

## Why This Bridge Matters

Without this bridge, Philotic risks becoming theoretically elegant and emotionally empty.

You already have agents with:

- personality
- continuity
- relationship context
- authored intent

Those should be brought forward, not discarded and later rediscovered with more ceremony and less history.

## Minimum Viable Bridge

The first bridge slice should support one imported agent end to end.

### Scope

1. read one agent entry from `openclaw.json`
2. resolve its workspace
3. ingest:
   - `SOUL.md`
   - `IDENTITY.md`
   - `USER.md`
   - `MEMORY.md`
4. store those as Philotic compatibility inputs
5. materialize that agent through Philotic
6. build prompt/context from the imported layers
7. verify it responds with recognizable identity

That is enough to prove the bridge is real.

## Prompt/Personality Bridge

For the first bridge, imported files should map to prompt/context layers like this:

- `SOUL.md` -> `soul_text`
- `IDENTITY.md` -> `identity_text`
- `USER.md` -> `user_context_text`
- `MEMORY.md` -> `memory_summary`

Those are compatibility fields, not the final model.

This lets Philotic preserve recognizable agent behavior immediately while still leaving room for later heuristic modeling.

## Memory Bridge

The bridge should not try to solve all memory backends at once.

For the first phase:

- import `MEMORY.md` as a curated memory seed
- keep dynamic retrieval separate
- let Philotic memory systems grow later without blocking agent import now

This matches the right principle:

- legacy memory files are seed input
- Philotic memory remains the future execution substrate

## User Continuity Bridge

`USER.md` should not remain just a static imported artifact forever.

Bridge recommendation:

- import it as initial user context
- attach it to the identified user where possible
- later let learned collaboration patterns extend it

That gives us continuity without freezing the relationship in markdown amber.

## Migration Philosophy

The bridge should allow us to:

- preserve legacy identity now
- improve internal modeling later
- avoid big-bang rewrites

So the migration path becomes:

1. import
2. materialize
3. verify
4. refine
5. replace internal assumptions gradually

This is the right kind of boring. Boring is underrated in migrations right up until the alternatives start sending Slack messages from the wrong soul.

## Immediate Next Slice

The recommended next implementation slice is:

1. add importer scaffolding for one OpenClaw agent
2. ingest `SOUL.md`, `IDENTITY.md`, `USER.md`, and `MEMORY.md`
3. add compatibility fields to the Philotic agent profile or session bootstrap path
4. refactor `agent-core` prompt assembly to use explicit layers
5. materialize the imported agent in Philotic and verify identity continuity

## Longer-Term Evolution

After the bridge is working, Philotic can safely evolve toward:

- heuristic personality modeling
- richer user adaptation
- layered memory retrieval
- structured agent profiles
- multi-user continuity

But that evolution should happen with your existing agents already alive in the new substrate, not as a prerequisite for letting them in.
