# Philotic Voice Machine Proposal

## Goal

Define a dedicated voice/media interface component for Philotic that can handle:

- speech-to-text
- text-to-speech
- speech-to-speech
- transcript generation
- media session routing

without turning `agent-core` into an audio pipeline with opinions.

## Disposition

Proposed and pinned for near-term design.

This work is not implemented yet, but it should be treated as an important upcoming interface subsystem rather than a distant optional enhancement.

Track active work in [task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Core Recommendation

Introduce a dedicated `voice machine` component that owns media pipeline selection and voice session handling.

It should decide, per interaction or per session, whether the right path is:

- `stt_tts`
- `speech_to_speech`
- `speech_to_speech_with_transcript`
- `text_only`
- hybrid text+audio output

## Why A Separate Component

Voice interaction is not just a transport formatting issue.

It has its own concerns:

- latency
- chunking/streaming
- interruption/barge-in
- transcript fidelity
- model capability matching
- audio artifact management

Those belong in a media-oriented component, not in the core cognitive loop.

## Pipeline Modes

### 1. STT -> Text Agent -> TTS

Use when:

- transcript quality matters
- text artifacts are required
- the selected model is text-native

### 2. Native Speech-to-Speech

Use when:

- the model supports direct speech input/output
- latency and conversational feel matter more than transcript-first processing

### 3. Speech-to-Speech With Transcript

Recommended long-term default when available.

Use when:

- native speech interaction is desired
- but canonical text artifacts still matter for:
  - approvals
  - memory
  - searchability
  - cross-interface continuity

## Canonical Session Recommendation

Even when speech-to-speech is used, Philotic should preserve text artifacts whenever possible.

The canonical session should remain symbolically inspectable:

- transcript
- summary
- media artifact refs
- timing/segment metadata

Not because text is sacred, but because systems become much easier to reason about when audio does not become the only surviving truth.

## Component Responsibilities

### Voice Machine

- media pipeline selection
- STT/TTS/S2S provider routing
- audio chunk handling
- interruption/barge-in handling
- transcript and media artifact generation
- emitting normalized turn/session payloads

### Hegemon

- transport-specific media ingress/egress
- handing voice messages to the voice machine
- delivery of audio/text results back to the user

### Agent Core

- cognition
- intent and tool planning
- no raw audio plumbing

## Open Design Questions

- how voice session identity relates to text session identity
- whether approvals can be handled by voice directly
- how partial speech output is interrupted or superseded
- how to store and reference media artifacts in the context graph
- how much transcript fidelity is required for memory and auditability

## Recommendation

- create a dedicated voice machine component
- make speech-to-speech a first-class pipeline mode from the start
- preserve canonical text artifacts whenever possible
- keep media routing separate from the core agent loop
