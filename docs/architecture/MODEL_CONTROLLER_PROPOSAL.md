---
title: "Model Controller Proposal"
doc_type: proposal
domain: tooling-execution
status: accepted-current-slice
last_updated: 2026-03-27
tags:
  - model-controller
  - models
  - oauth
  - voice
  - active-seam
related_docs:
  - ARCHITECTURE_STATUS.md
  - TASK_RUNNER_PROPOSAL.md
  - VOICE_MACHINE_PROPOSAL.md
  - KEY_VAULT_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: model-controller
implements: []
implemented_by:
  - structured-model-envelope-slice
  - gemini-oauth-guest-path-slice
  - voice-synthesize-envelope-slice
active_seams:
  - structured-model-envelope
  - hotel-gemini-oauth-flow
  - openai-provider-contract
  - hotel-openai-oauth-flow
  - provider-capability-overrides
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
---

# Model Controller Proposal

## Goal

Define a capability-addressed model-controller boundary for Philotic that can:

- route text generation, voice synthesis, and future multimodal response work through separate materialized model-controller guests
- support richer ElevenLabs audio capabilities beyond basic TTS
- support Gemini authentication via hotel-managed OAuth with API key fallback
- support OpenAI authentication via hotel-managed OAuth or API key, depending on provider path
- support OpenAI-standard model features without forcing Philotic to adopt OpenAI-specific worldview as its canonical schema
- keep provider invocation separate from higher-level delivery and transport concerns

## Core Recommendation

Model-controller requests should stay capability-addressed and provider-neutral at the API edge, while provider-specific options live in explicit extension fields.

They should also distinguish between the capability being requested and the kind of execution contract the request needs.

For the current direction:

- keep capability routing as the stable interface layer
- add a `request_class` field so the envelope can distinguish cognitive, transform, synthesis, and embedding work without splitting the controller boundary too early
- let model-controller implementations handle provider API invocation only
- add OpenAI as the next first-class provider through the existing `ModelProvider` seam instead of creating a parallel controller stack
- treat ElevenLabs as an audio-capability provider surface, not just a single `voice.synthesize` endpoint
- treat native multimodal voice-capable models as a separate response path, not as disguised ElevenLabs TTS
- let the hotel own OAuth UX, token storage, and token refresh
- let model-controller guests consume short-lived auth material from the hotel/config path
- let the hotel and session binding path project the effective rights and scoped execution posture; model-router may consume that posture, but must not inject new rights or widen the capability surface on its own
- keep API key support as the operational fallback for Gemini
- keep OpenAI-specific advanced features in explicit provider extensions until they prove they belong in the shared contract

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
- a first `request_class` split so cognitive calls can carry agent context and affordances without forcing every model call to pretend it is part of the reasoning loop
- a proposal-backed OpenAI provider slice covering standard text/tool/structured-output support, hotel-owned OAuth, and model-specific capability overrides

Current confidence for the implemented structured-envelope slice:

- `test-green`
  - `cargo test -p model-router -- --nocapture`
  - `cargo test -p philote -- --nocapture`
- `smoke-green` for the structured cognitive path
  - `bash scripts/smoke-cognitive-roundtrip.sh`
- not yet `watched-live-green` for end-to-end role/context projection in a live session

Linked task surface: [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)

## OpenAI Provider Recommendation

Philotic should add OpenAI support as the next provider on the existing `model-router` seam, not as a new orchestration layer.

That means:

- keep the canonical Philotic request/response envelope as the system contract
- add an OpenAI provider adapter that renders that contract into the OpenAI API surface
- map OpenAI outputs back into Philotic `ProviderOutput` variants
- keep advanced OpenAI-only behavior in explicit provider extensions rather than leaking it into every call path

The current code seam is already shaped for this:

- `ControllerTask` carries capability, request class, response contract, context, affordances, routing hints, and provider options
- `ModelProvider` already defines the provider boundary
- `ProviderOutput` already covers text, tool calls, audio artifacts, and embeddings

So the real work is capability normalization, auth shape, and honest handling of provider-specific features.

### Recommended initial OpenAI slice

Implement one OpenAI provider that supports the shared standard path first:

- `text.generate`
- tool calling
- structured outputs
- image-aware text generation / media analysis where the selected model supports it
- embeddings

Do not start with realtime, voice-native dialogue, or provider-built-in tools as if they were the same class of work. Those are follow-on slices.

### Why one provider first

The temptation will be to split immediately into:

- `model-controller-openai`
- `model-controller-openai-realtime`
- `model-controller-openai-embeddings`

That split may become correct later, but doing it first would be architecture cosplay.

The smallest honest slice is:

- one `OpenAIProvider`
- one standard OpenAI request renderer
- one standard response parser
- provider options for the first specialized features we actually need

If realtime or long-running/background behavior creates materially different lifecycle or streaming pressure later, then split the guest boundary then.

## Provider Standard vs Provider Extensions

Philotic should standardize the common execution contract, not erase provider differences that matter.

### Standardize in the shared contract

These should stay provider-neutral at the Philotic edge:

- capability names like `text.generate`, `voice.synthesize`, `media.analyze`, `text.embed`, `response.generate`
- `request_class`
- structured context layers
- structured response channels
- tool definitions and tool-call records
- attachment projection
- provider selection hints

### Keep in provider extensions first

These should live in `provider_options` until multiple providers justify promotion:

- reasoning effort / depth controls
- verbosity controls
- background or deferred response mode
- provider-native tracing handles
- OpenAI built-in tools such as web search, file search, code interpreter, or MCP tool surfaces
- realtime session parameters
- provider-native audio generation knobs

This is the important guardrail: provider-aware does not mean provider-owned. Philotic should learn from OpenAI's capabilities without letting one vendor's convenience surface become the canonical worldview.

Related guardrail: execution-aware does not mean rights-aware. `model-router`
may honor routing hints, model hints, and short-lived provider auth material,
but it must not become the place where new session rights appear because a
provider makes them easy to expose.

## OpenAI Auth Recommendation

OpenAI auth should support:

- hotel-managed OAuth where the OpenAI product/API path supports it
- API key fallback

Recommended auth modes:

- `api_key`
- `oauth_bearer`
- `oauth_refreshable`

Recommended boundary rule:

- the hotel owns browser login, token exchange, refresh, storage, and validation UX
- the guest consumes short-lived usable runtime auth material
- the guest must not become the owner of long-lived OAuth credentials

Acceptance criteria for this seam:

- hotel can start and validate an OpenAI auth flow without requiring guest restarts for every credential refresh
- model-router/provider config can prefer OAuth auth material over API key fallback when present
- failure is visible before API-key fallback quietly masks a broken OAuth path
- refreshable credentials live behind vault references rather than raw config values

## OpenAI Capability Override Recommendation

OpenAI model families expose materially different strengths and knobs.

Philotic should plan for provider-specific capability overrides such as:

- reasoning models with explicit effort controls
- models that only support a subset of tool or audio behavior
- realtime/audio-capable models that want a different runtime path
- built-in tool-enabled models whose behavior should be surfaced intentionally, not accidentally

Recommendation:

- keep the default OpenAI path focused on the standard shared contract
- add explicit feature gates and provider options for specialized models
- promote an OpenAI-specific feature into the shared Philotic contract only after at least one more provider or a clearly reusable internal surface wants the same thing

That keeps us from mistaking “OpenAI happens to expose this nicely” for “Philotic should now be this.”

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

Capability names alone are not enough to express what kind of execution contract a task needs. A `text.generate` request used for agent reasoning is not the same species of work as an embedding request or a narrow transform request, even when all of them happen to call a model.

For staged turns, the model-controller should remain the executor per stage, not the owner of the whole turn. A voice turn may legitimately traverse:

- `voice.transcribe` as `transform`
- `text.generate` as `cognitive`
- `voice.synthesize` as `synthesis`

with the session/agent still owning continuity, tools, approvals, and final response policy across the whole turn.

The next honest refinement is to make the capability species explicit instead of
leaving them as scattered strings. Current known turn-routed species should be
treated as:

- stage-local transform species:
  - `media.analyze`
  - `image.describe`
  - `document.summarize`
  - `voice.transcribe`
- stage-local cognitive species:
  - `text.generate`
- stage-local synthesis species:
  - `voice.synthesize`
  - `sound.generate`
  - `music.generate`
- collapsible native-live species:
  - `response.generate`
  - `voice.dialogue`

`response.generate` and `voice.dialogue` are the important native-live species.
They are allowed to collapse `ingress + cognition` under policy, but they
should not automatically become whole-turn owners. They still need explicit
envelope, approval, tool, and routing rules before they are allowed to bypass
the simpler staged pipeline.

Current implemented truth for this seam:

- `philote` now lets shared model markers plus routing preferences influence
  whether eligible voice turns keep the classic
  `voice.transcribe -> text.generate -> voice.synthesize` path or collapse into
  a native-live cognition species like `voice.dialogue`
- that chosen cognition species now flows through initial outbound request
  assembly and cognitive re-entry paths instead of being confined to plan
  metadata
- providers still must opt in explicitly; parsing and plan compilation now know
  about native-live species, but execution still refuses them until a provider
  implementation is wired honestly

### Gemini 3.1 Flash Live provider pressure

As of March 27, 2026, the official Gemini Live API docs and the Gemini 3.1
Flash Live model card make the next boundary pressure explicit:

- Gemini 3.1 Flash Live is a stateful Live API session, not a plain
  `generateContent` request/response call
- Live API audio input is raw PCM streamed with `send_realtime_input`
- audio output is returned as chunked model-turn parts
- Gemini 3.1 Flash Live uses sequential function calling, not non-blocking tool
  execution
- session management includes explicit lifecycle concerns like generation
  completion and session resumption

That means the current `ModelProvider::invoke(&ControllerTask) ->
ProviderOutput` seam is insufficient for a first-class Gemini Live provider on
its own. The smallest honest implementation path is:

1. keep `response.generate` / `voice.dialogue` as the routed capability species
2. add a session-shaped provider runtime under `model-router` for Live API work
3. let `philote` remain turn owner while the Gemini Live session acts as a
   stage-local execution enzyme
4. surface tool calls, partial replies, audio chunks, and turn-complete signals
   back through the canonical controller response/event contracts instead of
   letting the provider become a hidden parallel loop

The important guardrail is that Gemini Live should arrive as a session-shaped
execution substrate below the agent, not as a magical realtime exception that
quietly becomes the real orchestrator.

## Request Classes

Keep one model-controller boundary, but introduce an explicit `request_class` field inside the request envelope.

Recommended initial values:

- `cognitive`
  - agent reasoning calls that may use role posture, session context, tools, skills, and richer result channels
- `transform`
  - content interpretation or conversion calls such as transcription or media analysis, usually with narrow task-local instructions
- `synthesis`
  - artifact generation calls such as TTS, where the main output is an audio or media artifact rather than reasoning state
- `embedding`
  - vectorization/indexing calls where conversation-turn context is usually irrelevant

Recommended rule:

- `capability` answers: what does the caller want done?
- `request_class` answers: what kind of execution contract does this call require?

Do not create a separate cognitive process boundary yet unless runtime or operational pressure proves it is necessary. Separate the API contract first.

## Structured Request Envelope

Model-controller requests should move toward one canonical envelope with separate concerns for:

- `capability`
- `request_class`
- `response_contract`
- `context`
- `context_projection`
- `affordances`
- `routing_hints`
- `provider_options`

Recommended shape:

```json
{
  "capability": "text.generate",
  "request_class": "cognitive",
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
- per-stage routing hints without pretending the controller owns the entire multi-hop turn
- provider-specific rendering
- routing decisions

If everything becomes one giant prompt string, Philotic loses the ability to reason about what can be cached, dropped, summarized, or projected differently per model.

If everything becomes one giant cognitive envelope, Philotic also loses the ability to optimize transform, synthesis, and embedding work according to their actual needs.

Important invariant:

- structured envelopes must degrade cleanly to a minimal prompt-response path

That means the majority of request fields should remain optional.

The minimum honest request for `text.generate` should be able to look like:

```json
{
  "capability": "text.generate",
  "request_class": "cognitive",
  "context": {
    "active_turn": {
      "text": "Hello"
    }
  }
}
```

Everything else should be additive:

- `response_contract` only when extra output channels are desired
- `identity`, `memory`, and `dialogue_window` only when they are relevant
- `context_projection` only when the caller wants structured provenance/debuggability or model-side projection-aware behavior
- `affordances` only when skills or tools matter for this turn
- `routing_hints` only when selection needs steering
- `provider_options` only when provider-specific behavior is actually needed

If a simple prompt-response turn has to impersonate a fully dressed orchestration request, the schema has become more ceremonial than useful.

## Field Expectations By Request Class

Not every request class should carry the same fields.

### `cognitive`

Expected fields:

- `capability`
- `request_class`
- `context`
- optional `context_projection`
- optional `affordances`
- optional `response_contract`
- optional `routing_hints`
- optional `provider_options`

This is the class that should be allowed to carry:

- identity / relationship / session / working / knowledge context
- active role posture
- tool and skill affordances
- richer result channels such as `working_memory_delta`

### `transform`

Expected fields:

- `capability`
- `request_class`
- task-local prompt or instructions
- relevant attachments or inputs
- optional `routing_hints`
- optional `provider_options`

Usually avoid:

- full conversation-turn context
- tools/skills
- cognitive result channels

### `synthesis`

Expected fields:

- `capability`
- `request_class`
- source text or source artifact
- output/style options
- optional `response_contract`
- optional `routing_hints`
- optional `provider_options`

Usually avoid:

- agent identity and relationship context
- tool/skill affordances
- working-memory-oriented result fields

### `embedding`

Expected fields:

- `capability`
- `request_class`
- input text or batch inputs
- embedding/model options
- optional `routing_hints`
- optional `provider_options`

Usually avoid:

- conversation-turn context
- affordances
- response channels oriented around assistant replies

This split unlocks class-specific caching, routing, policy, and observability without fragmenting the controller boundary prematurely.

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

The same optionality rule should apply on the response side.

The minimum honest response for `text.generate` should be able to look like:

```json
{
  "capability": "text.generate",
  "result": {
    "display_text": "Hello"
  }
}
```

Other response fields should remain optional and appear only when:

- they were requested through `response_contract`
- they are naturally produced by the provider path
- downstream systems actually have a use for them

So:

- `spoken_text` is optional
- `working_memory_delta` is optional
- `follow_up_questions` are optional
- `artifacts` are optional
- `trace` should be available for observability, but consumers should not need it to extract the main answer

This keeps the structured response envelope useful for optimization without turning ordinary prompt-response behavior into a schema tax.

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

Policy note:

- transform stages should usually request narrow output, not planning/memory channels
- low-intent cognitive turns should be allowed to request a slimmer channel set than active problem-solving turns
- cognitive re-entry should reuse the same tool-projection policy as initial turns rather than bypassing it with a raw bound-tool replay
- stage-aware projection should also narrow prompt/context affordance cues such as skill guidance and approval posture, not just the explicit tool list
- approval interrupts should be stage-aware too: valid for real tool/workflow gates, redirected on low-intent conversational turns, and rejected on non-cognitive transform/synthesis stages
- routing self-improvement should be governed: the first `routing.policy.propose` path should surface a durable operator-reviewed proposal, not silently mutate controller choice or stage policy at runtime
- agent-local routing preferences can now flow in as advisory `routing_hints` from the active session binding path; model-controller may use those hints to honor provider/model bias, but it still must not become the authority for turn ownership or mutable routing policy
- model requests now also carry the projected `effective_rights` envelope so
  model-controller can validate that any surfaced tool contract is still inside
  the hotel's key ring, rather than trusting upstream assembly blindly

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
- `ansible auth openai start`
- `ansible auth openai validate`
- browser opens automatically
- hotel captures callback on localhost
- hotel confirms success
- Gemini model-controller begins using OAuth
- OpenAI model-controller begins using OAuth when the configured path supports it
- if OAuth is unavailable or expires irrecoverably, API key remains the fallback path

Operationally, the hotel should vend short-lived access tokens to the guest rather than permanently copying the long-lived credential into every model process.

## Deferred

This proposal intentionally does not yet define:

- the final media-delivery owner for audio artifacts
- whether expressive speech projection belongs in agent-core, a voice machine, or a delivery-stage postprocessor
- secret storage hardening details for refresh tokens
- exact context-graph schema for model auth material
- exact normalized envelope for music and sound-effect artifact metadata
- the final normalization strategy for OpenAI built-in tools versus Philotic-routed tools
- whether OpenAI realtime deserves its own materialized guest boundary or remains a provider mode inside the existing controller guest

## Near-Term Slice Recommendation

Implement the next slice in this order:

1. add the proposal-backed request envelope for `voice.synthesize`
2. add explicit `spoken_text` support and default-vs-override voice selection
3. add Eleven v3 model selection as a provider option
4. sketch `voice.dialogue`, `sound.generate`, and `music.generate` as capability stubs
5. add Gemini auth abstraction with OAuth-capable config shape and API key fallback
6. add hotel-side OAuth trigger and token handoff design before full implementation
7. add `OpenAIProvider` on the existing `ModelProvider` seam for text/tool/structured-output support
8. add hotel-side OpenAI auth trigger, token handoff design, and validation path before full implementation
9. add provider capability-overrides for reasoning effort and the first specialized OpenAI controls without broadening the shared contract prematurely
