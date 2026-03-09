# Model Controller Proposal

## Goal

Define a provider-facing `model-controller` seam so Philotic can add multiple model APIs
as independently materialized guests rather than baking providers into one guest binary.

## Core Recommendation

Treat `model-router` as the shared SDK/runtime crate for model-controller guests.

Boundaries:

- `model.manager.*` remains the owner of distributed model routing across the mesh
- each materialized `model-controller` guest owns one provider implementation or a deliberate provider bundle
- the future voice machine owns media pipeline selection, session behavior, and transport-facing audio delivery

This lets us add Gemini, ElevenLabs, and later providers under one provider seam without
pretending that voice orchestration is the same thing as provider invocation.

## Disposition

Accepted for current slice.

The provider abstraction lives inside `crates/model-router/` as shared guest runtime
infrastructure. The first separate controller binaries are `model-controller-gemini` and
`model-controller-elevenlabs`.

## Current Slice

- introduce shared model-controller runtime inside `crates/model-router`
- materialize Gemini and ElevenLabs as separate model-controller guest binaries
- route current text generation to the Gemini-specific guest role
- explicitly stop short of claiming end-to-end voice delivery until the voice machine and media path exist

Track active work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Why This Seam

The original `model-router` guest mixed together:

- provider-specific Gemini HTTP details
- controller responsibilities
- response-shaping shortcuts for the agent loop
- multiple providers inside one process, which prevents independent materialization

That is workable for one provider, but it fails the component boundary you actually want.
The first real slice should normalize around the API boundary we actually abstract and let
providers exist as separate mesh guests.

## Transitional Note

This is transitional architecture.

ElevenLabs as its own `model-controller` guest is acceptable for now as a provider
primitive, but it does not replace the dedicated [VOICE_MACHINE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/VOICE_MACHINE_PROPOSAL.md).

What is implemented in this slice:

- provider selection for text vs voice synthesis tasks
- provider-specific request building and response parsing
- separate guest roles so Gemini and ElevenLabs can scale independently on the mesh

What is not implemented in this slice:

- canonical media artifact storage
- audio delivery through `agent-core` and `hegemon`
- interruption/barge-in behavior
- transcript-first voice session handling
