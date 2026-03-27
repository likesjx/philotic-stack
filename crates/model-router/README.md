# `model-router` — Model Controller SDK + Guest Runtime

`model-router` is now the shared SDK/runtime crate for Philotic model-controller guests.
The mesh-level `model.manager.*` capability remains the owner of distributed routing.
Provider-specific model controllers materialize as separate guests on the mesh.

## Responsibilities

- Provide shared controller runtime and task parsing for materialized model guests
- Expose `model.manager.list@1` — enumerate available models across the mesh
- Expose `model.manager.route@1` — select best node for a given model + constraints
- Execute provider-specific model calls in separate guest binaries
- Return results as `TaskResult` events to the requesting agent
- Host transitional voice-provider work, such as ElevenLabs TTS, until the dedicated voice machine exists

## Local Invariants

Rules that prevent `model-router` bugs should live here, in nearby provider code, and in controller tests.

- Provider adapters preserve the canonical controller contract; provider quirks should not silently flatten it.
- Tool-enabled turns must preserve real tool schemas when the provider supports them.
- Non-tool text replies should return structured channels when those channels were requested.
- Provider-native features are allowed, but they must map back into the shared controller response envelope cleanly.
- Native-live providers must use the session-shaped live seam rather than pretending a stateful streaming protocol is just another one-shot `invoke()` call.

See:

- `src/controller.rs` for canonical request/response shaping
- `src/providers/gemini.rs` for Gemini-specific adaptation and schema preservation

## Routing Logic (MVP)

1. Receive `ModelRouteRequest { task, constraints, preferred_models }`
2. Query `NodeRegistry` for active nodes with matching `ModelRef`
3. Return first match as `ModelRouteResponse { model_ref, endpoint_node, invocation_params }`

Routing constraints supported: `latency_ms`, `privacy`, `cost_tier`.

## Current Materialized Controllers

- `model-controller-gemini` registers as role `model`
- `model-controller-elevenlabs` registers as role `model.elevenlabs`
- the legacy `model-router` binary remains as an all-in-one compatibility binary during transition

The Gemini path now has two explicit execution seams:

- classic one-shot provider invocation for `text.generate`, `media.analyze`, and `voice.transcribe`
- a native-live provider seam for `response.generate` / `voice.dialogue`

That live seam is now partially wired:

- `response.generate` can execute over the Gemini Live websocket path and return canonical text output
- `voice.dialogue` can execute over the Gemini Live websocket path and now prepares current blob-backed non-PCM audio through the shared `media-prep` enzyme, which currently uses a transitional ffmpeg-backed PCM path
- outbound audio now also uses a shared `audio_artifact` envelope from `media-prep`, so provider serialization and downstream delivery stop re-implementing the same payload contract three different ways
- native-live tool calls are intercepted before response-envelope serialization, just like the classic path
- session resumption handles are captured on the live seam

Important current limitations:

- cognitive-stage audio still re-enters the system as output transcription text rather than a first-class audio artifact
- the current shared `media-prep` PCM conversion enzyme still depends on host `ffmpeg` instead of an in-process codec substrate
- sequential in-session tool-response continuation is still a follow-on seam rather than fully implemented physiology

Voice synthesis is intentionally transitional here. The ElevenLabs guest can own provider
invocation, but first-class audio delivery still belongs to the future voice machine and
transport/media path.

## Running

Spawned automatically by `GuestManager`. For development:

```bash
cargo run -p model-router -- --ansible-port 9000
```

Or run a specific materialized controller:

```bash
cargo run -p model-router --bin model-controller-gemini
cargo run -p model-router --bin model-controller-elevenlabs
```
