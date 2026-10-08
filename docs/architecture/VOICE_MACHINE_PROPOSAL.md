---
title: "Philotic Voice Machine Proposal"
doc_type: proposal
domain: membrane-transport
status: accepted-current-slice
last_updated: 2026-10-08
tags:
  - voice
  - media
  - transcription
  - tts
  - active-seam
related_docs:
  - ARCHITECTURE_STATUS.md
  - AGENT_LOOP_PROPOSAL.md
  - TELEGRAM_INTEGRATION_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: voice-machine
implements: []
implemented_by:
  - policy-driven-voice-ingress-egress-slice
active_seams:
  - voice-transcribe-reentry
  - dedicated-voice-machine-component
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
---

# Philotic Voice Machine Proposal

## Goal

Define a dedicated voice/media interface component for Philotic that can handle:

- speech-to-text
- text-to-speech
- speech-to-speech
- transcript generation
- media session routing

without turning `philote` into an audio pipeline with opinions.

## Disposition

`accepted for current slice` — existing staged philote voice routing remains the foundation. The October 8 Apple source slice is implemented and test-green in isolated commits; complete private voice and end-to-end cancellation remain blocked on trusted runtime authority. Historical Telegram live evidence below does not verify this Apple slice.

## Current Slice — Apple voice (2026-10-08)

Implemented locally in `c4c3ff4d` and `46f871d2`: mandatory on-device recognition, stale permission/recognition/playback rejection, iOS duplex session configuration, request/turn-correlated client gates, Apple voice selection/preview/stop, and an ElevenLabs preference that grants no dispatch eligibility. External audio/STT/TTS entry paths and handsfree remain unavailable pending trusted policy installation. Ordinary local transcripts use the normal philote text turn; server-side always-speak policy is not yet proven private.

Backend checkpoint `9da6ba37` supplied the ledger; the source follow-up now reserves correlated submissions before IPC validation and echoes acceptance before generation. Verified-device cancellation admission and delivery/replay filtering are wired to a mock-tested runtime adapter seam. No production adapter is installed, so `turn_cancel_v1` stays unadvertised and real provider cancellation remains unavailable. Caller-supplied hotel GuestIdentity registration does not establish an authenticated principal.

Verification: 19 app source tests; 135 PhiloticKit tests with one live test skipped and zero failures; shared macOS app source typecheck; generic iOS simulator build for arm64/x86_64; five standalone Rust ledger tests. The ledger's committed-tool fixture is not proof of runtime tool cancellation semantics. Backend follow-up adds 30 protocol tests, 30 edge unit tests, ten ledger tests, three synthetic profile tests, all 12 fake-hotel loopback WebSocket tests, and a successful all-target web compile check. Running mock-job cancellation preserves its committed tool fixture. No simulator execution, physical-device audio, real microphone, live provider or deployed behavior was verified.

Blocked next work: verified hotel launch/session authority and opaque task handles across direct/parked/reparked delivery and SDK decoding; guarded model-router privacy dispatch; authenticated runtime cancellation through retries and active streams. Cancellation must stop future generation/publication without undoing committed tools. Private or unknown provenance denies external processing, with no external fallback after local failure.

Independent lifecycle cleanup now separates active turns from expiring terminal metadata and preserves bounded request fingerprints across reconnect; its 4,096-request safe budget fails closed. Profile metadata preparation is mock-tested and uninstalled.

Open after authority: eligible ElevenLabs expressive profiles and previews, fresh/reconnect/interruption integration tests, macOS shared duplex engine/AEC, real permission and route validation, and browser membrane parity. iPad received shared-source compilation only; broad iPad redesign is deferred. Dedicated voice-machine materialization remains deferred.

See the [client contract](../../apps/philotic-apple/PhiloticApp/Tests/VOICE_CLIENT_CONTRACT.md), [backend contract](../../crates/philotic-web/src/serve/VOICE_TURN_BACKEND_CONTRACT.md), and [active tasks](../task.md#apple-voice-privacy-and-cancellation).

Track active work in [task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

---

## First Implementation Slice (landed on `codex/membrane-membrane-slice`)

Before the full voice machine component exists, two policy-driven seams were added to the existing pipeline:

### Inbound: configurable media routing (`MediaRoutingPolicy`)

Added to `AgentProfile` in `agent-core/src/session.rs`. Controls what happens when a blob-backed media attachment arrives from membrane:

| Field | Default | Effect |
|---|---|---|
| `forward_media_to_model` | `true` | When `false`, strip all attachments and treat turn as text-only |
| `voice_action` | `None` → `"analyze_media"` | Action/capability used for voice/audio attachments |
| `image_action` | `None` → `"analyze_media"` | Action/capability used for photo/image attachments |
| `document_action` | `None` → `"analyze_media"` | Action/capability used for document attachments |

Action → capability mapping: `"transcribe"` → `voice.transcribe`, `"describe"` → `image.describe`, `"summarize"` → `document.summarize`, anything else → `media.analyze`.

Tool suppression is no longer configured here. Media routing picks the ingress action; staged turn routing owns whether a given stage sees tools at all.

`model-router` gained `TaskKind::AudioTranscribe` (`voice.transcribe`). Gemini handles it via the same inline-bytes path, using a transcription-focused prompt. A dedicated STT guest can be wired by pointing the `voice.transcribe` component route at it.

### Outbound: policy-driven TTS (`VoiceResponsePolicy`)

Added to `AgentProfile`. Controls whether the agent synthesises speech for its responses:

| Field | Default | Effect |
|---|---|---|
| `mode` | `"off"` | `off`, `auto`, or `on` |
| `provider` | `None` | Provider hint (e.g. `"elevenlabs"`) |
| `voice_id` | `None` | The agent's permanent voice identity |
| `model` | `None` | Provider model override |
| `speed_percent` | `None` | Speech rate override; `100` means normal speed |
| `send_text_caption` | `true` | Also deliver text alongside audio when mode is `on` |
| `fallback_to_text` | `true` | Text-only delivery if synthesis fails |

Current expected UX for the Telegram agents:

- `mode = "auto"` means a voice memo should get a voice-only reply by default
- `/tts on` switches to `mode = "on"` and should deliver both voice and text
- `/tts off` switches text turns back to text-only, but voice memos should still get voice-only replies
- `send_text_caption` is ignored in `auto` mode on purpose, so mirrored voice replies stay voice-only

Pipeline: model responds → `complete_agent_response` checks policy → `start_voice_synthesis` stashes text, sets `TurnPhase::WaitingVoice`, emits `voice.synthesize` to the voice component → response returns via `handle_voice_synthesis_response` → `deliver_text_reply` sends `FinalReplyPayload` (with `audio_artifact`) to membrane → membrane calls `sendVoice`/`sendAudio` via Telegram multipart.

Current agent-core requirement:

- `voice.transcribe` is an intermediate transform, not the final assistant answer
- the transcript must be routed back into the normal reasoning loop as the user turn
- only the post-reasoning assistant reply should flow into `voice.synthesize`

### Current turn-routing slice

`philote` now treats a voice turn as a staged execution contract instead of a lucky branch:

1. `ingress` — `voice.transcribe` as a `transform` request
2. `cognition` — `text.generate` as the normal agent reasoning turn
3. `egress` — `voice.synthesize` as a `synthesis` request when voice reply policy is active

That plan is compiled at turn start, stored on the active turn, checkpointed with session recovery state, and surfaced in task progress updates (`waiting_model`, transcription re-entry `waiting_model`, and `waiting_voice`).

The current implementation now also uses the stage plan at request assembly time:

- ingress transform calls get a slimmer context envelope with no tool history, no recalled memory, and only a minimal recent dialogue window
- non-cognitive stages suppress tool projection entirely, even if the session has tools bound
- cognitive calls keep the full reasoning envelope
- cognitive re-entry now respects the same projection policy instead of re-exposing the full bound toolset by accident
- low-intent cognitive turns now ask for fewer side channels, hide skill guidance, and replace detailed approval posture with a simple direct-reply steer
- inappropriate free-form approval interrupts now redirect low-intent cognitive turns back to direct response and reject non-cognitive stages instead of surfacing stray approval cards
- reflexive routing refinement now has its first governed hook: `routing.policy.propose` plus the `routing.refinement` abstract skill let the agent surface repeatable routing-policy changes for operator review instead of silently rewriting its own turn-routing reflexes
- stored agent-graph routing preferences are now projected into session bindings and applied as advisory provider/model overrides during turn-plan compilation, so voice ingress/cognition/egress can start honoring learned agent-local posture without confusing the shared model graph for mutable preference state

Current transitional reality:

- routing-policy proposals are semantically distinct from general behavior rules
- persistence is now distinct: they store as dedicated `routing_policy` records with operator disposition and evaluation history
- operator disposition is no longer birth-only; the hotel can now list and later revise routing-policy disposition while appending durable disposition evaluations
- this keeps the improvement loop real without pretending the general rule store should own routing reflexes
- model requests now carry stage-derived `routing_hints` so model-controller can see provider preference, model hint, controller role, capability, and envelope intent without becoming the owner of the turn

This is intentionally still transitional:

- the stage plan is turn-local observability and execution intent, not a second routing authority
- component-route resolution and provider invocation still belong to the existing model-controller/runtime seams
- context envelopes are now stage-aware at request assembly time, but the deeper payload-builder split is still transitional

### Example config

```json
{
  "agent_profile": {
    "media_routing_policy": {
      "voice_action": "transcribe",
      "image_action": "analyze_media"
    },
    "voice_response_policy": {
      "mode": "auto",
      "provider": "elevenlabs",
      "voice_id": "YOUR_ELEVENLABS_VOICE_ID",
      "model": "eleven_multilingual_v2",
      "speed_percent": 92,
      "send_text_caption": true,
      "fallback_to_text": true
    }
  }
}
```

### What this is NOT yet

- No dedicated voice machine guest/component — the policy targets existing model guests (`model.elevenlabs`, `model.gemini`)
- No streaming or chunked audio
- No interruption/barge-in
- No speech-to-speech (STT + TTS are separate hops)
- No media artifact storage in the context graph

---

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
- the session should preserve one canonical agent-owned turn across the whole flow

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

### Membrane

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
- how the dedicated transcription task should be finalized as a first-class bounded media transform rather than an ad hoc side path

## Recommendation

- create a dedicated voice machine component
- make speech-to-speech a first-class pipeline mode from the start
- preserve canonical text artifacts whenever possible
- keep media routing separate from the core agent loop
