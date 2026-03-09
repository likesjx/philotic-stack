# Model Controller Proposal

## Goal

Define a capability-addressed model-controller boundary for Philotic that can:

- route text generation, voice synthesis, and future multimodal response work through separate materialized model-controller guests
- support richer ElevenLabs audio capabilities beyond basic TTS
- support Gemini authentication via hotel-managed OAuth with API key fallback
- keep provider invocation separate from higher-level delivery and transport concerns

## Core Recommendation

Model-controller requests should stay capability-addressed and provider-neutral at the API edge, while provider-specific options live in explicit extension fields.

For the current direction:

- keep capability routing as the stable interface layer
- let model-controller implementations handle provider API invocation only
- treat ElevenLabs as an audio-capability provider surface, not just a single `voice.synthesize` endpoint
- treat native multimodal voice-capable models as a separate response path, not as disguised ElevenLabs TTS
- let the hotel own OAuth UX, token storage, and token refresh
- let model-controller guests consume short-lived auth material from the hotel/config path
- keep API key support as the operational fallback for Gemini

## Disposition

Accepted for current slice.

## Current Slice

Pin and prove the first design contract for:

- ElevenLabs capability surface and request envelope
- expressive speech text vs user-visible text
- native-audio multimodal model support
- hotel-driven Gemini OAuth UX
- an upstream producer path for `voice.synthesize`
- a guest-side Gemini auth abstraction that prefers OAuth bearer material over API key fallback
- a hotel-side Gemini OAuth validation path that proves stored auth can call a real Gemini model
- a structured model request envelope that separates context layers from routing hints and provider options
- a structured model response envelope with explicit response channels for optimization-oriented outputs

Linked task surface: [docs/task.md](/Users/jaredlikes/code/philotic-stack-model-controller-abstraction/docs/task.md)

## Capability Surface

The model-controller API should be structured around capabilities such as:

- `text.generate`
- `voice.synthesize`
- `voice.dialogue`
- `sound.generate`
- `music.generate`
- `speech.transcribe`
- `response.generate`

`response.generate` is the important escape hatch for multimodal models that can emit text and audio directly. It should not be forced through the `voice.synthesize` path, because that would collapse native model audio into a fake TTS abstraction.

## Structured Request Envelope

Model-controller requests should move toward one canonical envelope with separate concerns for:

- `capability`
- `response_contract`
- `context`
- `routing_hints`
- `provider_options`

Recommended shape:

```json
{
  "capability": "text.generate",
  "response_contract": {
    "modalities": ["text"],
    "style": "assistant_reply"
  },
  "context": {
    "instructions": [],
    "identity": [],
    "memory": [],
    "dialogue_window": [],
    "active_turn": {
      "role": "user",
      "parts": [{ "type": "text", "text": "..." }]
    },
    "attachments": []
  },
  "affordances": {
    "skills": [],
    "tools": []
  },
  "routing_hints": {
    "implementation": "gemini"
  },
  "provider_options": {}
}
```

This gives the system a stable capability-facing request while preserving structured seams for:

- prompt projection
- context trimming
- cache reuse
- provider-specific rendering
- routing decisions

If everything becomes one giant prompt string, Philotic loses the ability to reason about what can be cached, dropped, summarized, or projected differently per model.

## Structured Response Envelope

The response side should follow the same philosophy: do not collapse every model result back into a single text field.

Recommended top-level response shape:

- `capability`
- `result`
- `artifacts`
- `trace`
- `provider_output`

Recommended example:

```json
{
  "capability": "text.generate",
  "result": {
    "display_text": "...",
    "spoken_text": null,
    "working_memory_delta": null,
    "follow_up_questions": []
  },
  "artifacts": [],
  "trace": {
    "provider": "gemini",
    "model": "gemini-2.5-flash"
  },
  "provider_output": {}
}
```

This should not be a naive mirror of the request envelope just for aesthetic symmetry.

The response envelope exists to separate:

- what the user should see
- what downstream delivery systems should perform
- what the runtime should retain for tighter future turns
- what machine-readable hints the agent or UI can use next
- what provenance/provider detail should be preserved without polluting the main semantic result

If the request becomes structured but the response falls back into a glorified string return value, Philotic keeps only half of the optimization seam.

## Response Channel Recommendation

The response contract should be able to request explicit result channels such as:

- `display_text`
- `spoken_text`
- `working_memory_delta`
- `follow_up_questions`
- `intent_summary`
- `state_updates`
- `delivery_hints`

These should be requested through `response_contract`, not smuggled in through prompt folklore.

Recommended shape:

```json
{
  "response_contract": {
    "modalities": ["text"],
    "channels": [
      "display_text",
      "spoken_text",
      "working_memory_delta",
      "follow_up_questions"
    ]
  }
}
```

This allows a caller to say:

- ordinary text reply only
- text plus expressive speech projection
- text plus session-tightening memory delta
- text plus suggested next questions

without pretending every turn needs every output.

## Optimization-Oriented Response Channels

### `working_memory_delta`

`working_memory_delta` should be a compact, bounded summary of what should remain active for the next turns.

Why:

- it helps keep a session tight without replaying the whole transcript
- it can feed session compaction or short-horizon working memory
- it is more honest than re-scraping the assistant reply later for state

Guardrails:

- it should summarize active state, not rewrite durable memory authority
- it should be bounded and structured enough to avoid essay sprawl
- it should be optional per turn

### `spoken_text`

`spoken_text` should be the expressive performable form of the reply.

Why:

- it lets the text model emit delivery-aware speech text directly
- it gives ElevenLabs and later voice systems a better substrate than raw display prose
- it keeps performance markup and spoken cadence out of the user-visible text

This channel aligns directly with the earlier ElevenLabs recommendation.

### `follow_up_questions`

`follow_up_questions` should be a list of useful next questions or missing-information prompts.

Why:

- it helps guide future turns
- it can power UI suggestion chips or agent steering
- it makes uncertainty and missing inputs explicit

This is a cleaner seam than hoping the model casually remembers to ask smart questions in prose.

### `intent_summary`

`intent_summary` should capture the current user ask in one concise line.

Why:

- useful for routing, labeling, and session summaries
- can help session management and memory indexing
- can be easier to trust downstream than scraping the whole response

### `state_updates`

`state_updates` should carry structured facts or decisions that the caller may want to merge into working or session state.

Why:

- this avoids reparsing prose to recover operational state
- it creates a seam for bounded session updates without making the reply itself do all the jobs

### `delivery_hints`

`delivery_hints` should capture output-shaping metadata such as:

- tone
- intensity
- pace
- style hints

These should stay provider-neutral where possible and only fall into provider-specific formatting at the projection layer.

## Response Boundary Recommendation

Important pushback:

- do not ask the model to emit every structured channel on every turn
- do not let `working_memory_delta` become a shadow canonical memory store
- do not force provider-specific audio markup into `display_text`
- do not treat follow-up suggestions as the same thing as the main answer

The response contract should select only the channels that matter for the current turn, and the runtime should be able to ignore channels it did not ask for.

## Context Layer Recommendation

The canonical `context` object should be layered as:

1. `instructions`
2. `identity`
3. `memory`
4. `dialogue_window`
5. `active_turn`
6. `attachments`

Why this split:

- `instructions` are policy, task framing, and other higher-priority behavior constraints
- `identity` is who the agent is
- `memory` is recalled relevant knowledge, not full transcript continuity
- `dialogue_window` is recent conversational context
- `active_turn` is the thing being answered right now
- `attachments` are non-text inputs that may need provider-native rendering

Important pushback:

- do not collapse `memory` and `dialogue_window` into one history blob
- do not treat `active_turn` as just “the last message”
- do not let routing hints or provider flags leak into the semantic context object

Those categories want different projection and trimming policies. If they are fused, optimization becomes mostly ceremonial.

## Tools And Skills Recommendation

Yes, tools and skills should be separated out, but not as peer context layers.

They should live in a distinct `affordances` section because they are neither identity nor memory nor dialogue.

Recommended split:

- `skills`
  - procedural or instructional overlays that shape how the model should approach the task
- `tools`
  - executable affordances the runtime can actually satisfy

Why:

- skills are interpretive guidance
- tools are actionable capabilities
- both affect model behavior
- neither should be confused with recalled semantic context

So the recommendation is:

- `context` answers: who am I, what matters, what is happening now
- `affordances.skills` answers: what approach patterns should I follow
- `affordances.tools` answers: what can I actually invoke

This also keeps future optimization cleaner:

- skills can be projected sparsely or omitted when irrelevant
- tools can be narrowed to the active tool assembly instead of dumping full inventory
- both can be rendered differently per provider without corrupting the semantic context model

Guardrail:

- do not treat `AGENTS.md`-style operational guidance as ordinary conversational context
- do not dump every tool and skill into every request just because they exist
- expose them through turn-time projection only when relevant to the current goal

## Projection Metadata Recommendation

Each projected context or affordance item should eventually carry metadata such as:

- `source_ref`
- `projection_kind`
- `priority`
- `token_estimate`
- `cache_key`
- `truncation_policy`

That metadata is what will let Philotic optimize model usage honestly instead of pretending string concatenation is architecture.

## ElevenLabs Recommendation

ElevenLabs should expose a wider family of capabilities than the current single-speaker TTS slice.

Initial capabilities to model:

- `voice.synthesize`
  - single-speaker speech from text
- `voice.dialogue`
  - multi-speaker dialogue generation, especially for Eleven v3 dialogue mode
- `sound.generate`
  - sound effects generation
- `music.generate`
  - music generation

For `voice.synthesize`, upstream callers should be able to pass a voice override, while the hotel/context graph provides a pinned default voice.

Recommended request shape:

- `text`
  - user-visible text or source text
- `spoken_text`
  - optional expressive speech script when different from the user-visible text
- `voice`
  - optional explicit voice selector from upstream
- `model`
  - optional provider model selection such as `eleven_v3`
- `audio_format`
  - mp3/pcm/etc.
- `provider_options`
  - explicit provider extension bag for non-portable settings

Voice resolution order:

1. request voice override
2. session/component binding
3. hotel default voice
4. provider fallback

This keeps the default pinned while still allowing upstream voice selection.

## Expressive Text Recommendation

Philotic should support both:

- `display_text`
  - what the user reads
- `spoken_text`
  - what the voice engine actually performs

Why:

- expressive TTS often needs stage direction, timing, and audio tags that do not belong in the plain user-facing reply
- some systems may want to generate a concise readable reply plus a richer performable script

For Eleven v3 specifically, `spoken_text` is the correct place for inline audio tags and expressive delivery markup. This should be treated as a provider-aware projection, not as the canonical user-visible response.

Guardrail:

- the system should preserve semantic alignment between `display_text` and `spoken_text`
- `spoken_text` may enrich delivery, but should not silently change intent

## Eleven v3 Recommendation

Eleven v3 should be supported as an explicit model option, not hidden behind a generic TTS switch.

Why:

- it introduces stronger expressive control via inline audio tags
- it supports dialogue generation
- it is better for high-expression output than plain low-latency conversational use

Current external reality from ElevenLabs docs:

- Eleven v3 supports audio tags and dialogue mode
- ElevenLabs documents sound-effects and music APIs as separate surfaces
- ElevenLabs guidance indicates v3 is more expressive but less suitable for realtime conversational use than faster models

So Philotic should expose:

- fast voice path for low-latency/conversational audio
- expressive voice path for high-fidelity performed audio

That distinction is architectural, not just a model string.

## Multimodal Models With Native Voice

Philotic should also support models that produce text and audio natively.

Examples from current official docs:

- Gemini Live / native audio model family
- OpenAI realtime/audio-capable models

These should map to `response.generate` with output modalities such as:

- `text`
- `audio`
- `text+audio`

This path should allow the model to emit:

- `display_text`
- audio artifact
- optional transcript/alignment metadata
- optional native voice metadata

This is distinct from provider-chaining:

- native multimodal audio: one model produces both
- TTS pipeline: one model produces text, another provider voices it

Philotic should support both paths explicitly.

## Gemini Auth Recommendation

Gemini auth should support:

- hotel-managed OAuth
- API key fallback

Recommended auth modes:

- `api_key`
- `oauth_bearer`
- `oauth_refreshable`
- `adc`

Current implementation slice:

- model-controller guest can consume `gemini_oauth_access_token`
- model-controller guest can resolve `gemini_oauth_access_token_ref` through hotel secret IPC
- optional `gemini_oauth_project_id` is forwarded as `x-goog-user-project`
- if OAuth material is absent, the guest falls back to `gemini_api_key`
- refreshable credentials still belong to the future hotel/vault flow, not the guest
- hotel CLI now has a transitional `auth google start --provider gemini` loopback flow
- access-token updates can take effect on the next model request because provider config is refreshed per task
- hotel CLI can validate the stored Gemini OAuth path with a real `generateContent` call

Acceptance criterion for this seam:

- hotel completes Google OAuth
- hotel stores auth material safely enough for the current slice
- a real Gemini model call succeeds using the OAuth path
- failure is observable before fallback-to-api-key silently hides it

The hotel should own the user experience for OAuth:

1. operator triggers auth from the hotel CLI
2. hotel opens the browser to Google login
3. hotel starts a temporary localhost callback listener
4. Google redirects with the authorization code
5. hotel exchanges the code for access and refresh tokens
6. hotel stores the refreshable credential in the canonical config/secret plane
7. hotel provides usable short-lived access tokens to the Gemini model-controller guest

Important boundary:

- do not make the model-controller guest own the browser login flow
- do not rely on a one-time bearer token with no refresh story
- do not make the guest the canonical owner of long-lived OAuth credentials
- do not quietly persist refresh tokens in plain `node_config` as if that were a security design

The hotel is the better authority because it already owns local runtime coordination and canonical configuration.

## OAuth UX Recommendation

Target operator experience:

- `ansible auth google start --provider gemini`
- `ansible auth google validate --provider gemini`
- browser opens automatically
- hotel captures callback on localhost
- hotel confirms success
- Gemini model-controller begins using OAuth
- if OAuth is unavailable or expires irrecoverably, API key remains the fallback path

Operationally, the hotel should vend short-lived access tokens to the guest rather than permanently copying the long-lived credential into every model process.

## Deferred

This proposal intentionally does not yet define:

- the final media-delivery owner for audio artifacts
- whether expressive speech projection belongs in agent-core, a voice machine, or a delivery-stage postprocessor
- secret storage hardening details for refresh tokens
- exact context-graph schema for model auth material
- exact normalized envelope for music and sound-effect artifact metadata

## Near-Term Slice Recommendation

Implement the next slice in this order:

1. add the proposal-backed request envelope for `voice.synthesize`
2. add explicit `spoken_text` support and default-vs-override voice selection
3. add Eleven v3 model selection as a provider option
4. sketch `voice.dialogue`, `sound.generate`, and `music.generate` as capability stubs
5. add Gemini auth abstraction with OAuth-capable config shape and API key fallback
6. add hotel-side OAuth trigger and token handoff design before full implementation
