---
title: Unified Capability Stream
doc_type: proposal
domain: architecture
status: draft
last_updated: 2026-05-27
tags:
- architecture
- primitives
- vision
- audio
- text
- streaming
- capability-provider
- model-router
- philote
related_docs:
- MODEL_GRAPH_FLYWHEEL_PROPOSAL.md
- ARCHITECTURE.md
proposal_id: unified-capability-stream
supersedes: []
implements: []
implemented_by: []
---

# Unified Capability Stream

## Problem

The stack has two parallel execution paths for model capabilities, and they are growing further apart with every new feature:

| Path | Interface | Protocol | Invocation |
|---|---|---|---|
| **Model path** | `ModelProvider` + `run_model_controller` | IPC request/response, streaming chunks | Automatic (hotel dispatches on message arrival) |
| **Datasource path** | `DatasourceProvider` + `run_datasource_controller` | Action dispatch, single JSON response | Tool-invocable (LLM calls via tool use) |

The split made sense initially — generation is streaming, tool results are not. But it created structural problems:

1. **Any model-controller can only live on one path.** `model-controller-onnx` handles `text.embed`, `audio.transcribe`, `voice.synthesize` on the model path. `model-controller-vision` handles `image.ocr`, `image.ground` on the datasource path. They share the same ONNX runtime (`onnx-runner`) but cannot share a binary or a registration mechanism.

2. **`ModelProfileRecord.task_kinds` is the routing reflex, but datasource tools bypass it.** The health-aware routing reflex in Slice 3 routes generation tasks by querying which active model-controller has a given `task_kind` registered. Datasource-path tools (vision, table) use hardcoded `target_role` strings in philote instead. Two routing mechanisms for the same conceptual problem.

3. **Adding a new modality means touching both paths.** A new `audio.classify` capability requires: a datasource binary, its own IPC handlers in aiua, routing code in `philote/src/session/mod.rs`, and a separate `ModelProfileRecord` registration. The model path adds the same volume of boilerplate in different files.

4. **Context prep is scattered.** Image URL fetching happens in the datasource provider binary. Audio transcoding happens in `OnnxProvider`. There is no shared pipeline for normalizing inputs before dispatch.

## Core Insight

**Primitives are abstract capabilities. Any model-controller that can serve a primitive declares it. The routing layer picks the healthiest available provider.**

`image.ocr` is not "the vision binary's job." It is a capability that `model-controller-onnx` (Florence-2), `model-controller-gemini` (Vision API), and `model-controller-openai` (GPT-4V) can each implement. When the LLM calls `image.ocr`, the health-aware reflex picks the best available provider — local ONNX first, cloud fallback automatically.

This insight already works for generation: `text.generate` can be served by Gemini, OpenAI, or Ollama, and the reflex picks based on health and priority. The goal is to extend this to ALL capability types — perception, synthesis, embedding, transformation.

## Design

### 1. Primitive Namespace

Every capability is named `{modality}.{operation}`:

| Namespace | Tool-invocable (LLM calls) | Automatic/pipeline (hotel dispatches) |
|---|---|---|
| `image.*` | `image.ocr`, `image.describe`, `image.ground`, `image.classify` | — |
| `audio.*` | `audio.transcribe`, `audio.classify`, `audio.describe` | — |
| `voice.*` | — | `voice.transcribe` (on message arrival), `voice.synthesize` (TTS output) |
| `text.*` | `text.search`, `text.classify` | `text.generate`, `text.embed` |
| `doc.*` | `doc.extract`, `doc.ocr` | — |

**Rule:** anything the LLM explicitly calls to _understand_ media → tool-invocable. Anything the hotel triggers automatically (on message arrival or response output) → automatic pipeline.

### 2. CapabilityProvider Trait

A single unified trait replaces `ModelProvider` and `DatasourceProvider`:

```rust
#[async_trait]
pub trait CapabilityProvider: Send + Sync {
    fn id(&self) -> &str;

    /// All task_kinds this provider can serve.
    fn capabilities(&self) -> &[&str];

    fn supports(&self, task_kind: &str) -> bool {
        self.capabilities().contains(&task_kind)
    }

    /// Invoke and return a stream of capability events.
    /// Non-streaming providers emit a single `CapabilityEvent::Result` + `Done`.
    async fn invoke_stream(
        &self,
        req: &CapabilityRequest,
    ) -> Result<impl Stream<Item = Result<CapabilityEvent>>>;
}
```

Each binary calls a unified `run_capability_controller` runtime (replacing both `run_model_controller` and `run_datasource_controller`).

### 3. Normalized Input Envelope

All capability invocations share one request type, regardless of modality:

```json
{
  "task":    "image.ocr",
  "inputs":  [
    { "type": "image", "url": "file:///tmp/screenshot.png" },
    { "type": "text",  "content": "extract the table only" }
  ],
  "context": {
    "conversation_id": "...",
    "agent_id":        "bjork",
    "identity":        { ... }
  },
  "constraints": {
    "format":     "json",
    "max_tokens": 2000
  }
}
```

The same envelope works for `text.generate` (inputs = conversation turns), `audio.transcribe` (inputs = audio attachment), `image.ground` (inputs = image + text query). Model-controllers extract what they need and ignore the rest.

### 4. Philote Pipeline (Option C — Hybrid)

Philote handles input preparation (stages 1–3). The hotel handles routing and streaming relay (stages 4–6).

```
LLM tool call arrives
        │
  [1] resolve_primitive
        resolve task_kind from tool name + schema
        │
  [2] prepare_inputs                          ← runs in philote process
        fetch image URLs, decode base64,
        resize/transcode if needed,
        chunk long text
        │
  [3] merge_context
        inject conversation window,
        agent identity, rights envelope
        │
  IpcRequest::CapabilityRequest { ... }       ← sent to hotel via IPC
        │
  [4] route                                   ← hotel queries ModelProfileRecord
        pick healthiest provider for task_kind
        │
  [5] dispatch_stream
        hotel forwards to model-controller,
        receives streaming events
        │
  [6] relay + accumulate                      ← hotel relays to philote
        philote collects until Done,
        returns result to LLM as tool result
```

Stages 2 and 3 handle multi-modal composition: a call to `image.ground` with a text `query` merges both modalities in stage 3 so the model-controller sees a unified input. Stage 2 handles all mechanical prep (download, decode, format) so the controller never touches raw URLs.

### 5. Streaming Event Protocol

Every capability invocation produces the same event stream, regardless of whether the underlying task is generative or perceptive:

```json
{ "event": "chunk",  "delta": "some text..." }          // generative: text chunk
{ "event": "result", "data": { ... } }                  // perceptive: single result
{ "event": "tool",   "call": { "name": "...", ... } }   // if controller sub-calls
{ "event": "done",   "usage": { ... } }                 // always last
{ "event": "error",  "message": "..." }                 // if failed
```

Non-generative primitives (`image.ocr`) emit `result` + `done`. Generative ones (`text.generate`) emit `chunk` × N + `done`. Philote's accumulator handles both identically — it collects until `done`.

This eliminates the current split between "streaming chunks from model path" and "single JSON ack from datasource path."

---

## Migration Slices

### Slice 5 — Proof Path: ONNX-Native Vision ✅ COMPLETE
**Status**: Shipped (this session)

- Added `VisionBackend` to `onnx-runner` using Florence-2 ONNX
- Added `pull_vision()` to `ModelCache` (hub.rs)
- Replaced Python `ScriptProvider` in `model-controller-vision` with `OnnxVisionProvider`
- No Python dependency; ONNX inference via `ort` with CoreML acceleration on Apple Silicon
- Validates: "any model-controller can serve any primitive using the shared ONNX backend"

Files changed:
- `crates/onnx-runner/Cargo.toml` — added `image` dep
- `crates/onnx-runner/src/hub.rs` — `VisionHandle`, `pull_vision()`
- `crates/onnx-runner/src/backends/vision.rs` — `VisionBackend` (new)
- `crates/onnx-runner/src/lib.rs` — re-exports
- `crates/model-router/src/bin/model-controller-vision.rs` — replaced `ScriptProvider`

---

### Slice 6 — Define CapabilityRequest/Event Types
**Status**: Planned | Est: 1 day

Add to `ansible-mesh-core`:
- `CapabilityRequest` struct (normalized input envelope)
- `CapabilityInput` enum (`Text`, `Image`, `Audio`, `Document`)
- `CapabilityEvent` enum (`Chunk`, `Result`, `Tool`, `Done`, `Error`)
- `CapabilityConstraints` struct

Also add `IpcRequest::CapabilityRequest` to `philotic-client`.

No routing or binary changes in this slice — types only.

---

### Slice 7 — Hotel Streaming Capability Router
**Status**: Planned | Est: 2 days

Implement hotel-side routing and streaming relay in `aiua/src/service/ipc.rs`:

1. Add handler for `IpcRequest::CapabilityRequest`
2. Query `ModelProfileRecord` by `task_kind` — same health-aware reflex as generation routing
3. Select model-controller guest by priority + health
4. Forward `CapabilityRequest` to selected guest's IPC socket
5. Stream `CapabilityEvent` frames back to the requesting philote

This makes the hotel a **streaming capability router** — model-controllers and LLM agents are both its clients.

---

### Slice 8 — Philote Pipeline (Stages 1–3)
**Status**: Planned | Est: 2 days

Replace the scattered dispatch logic in `philote/src/session/mod.rs`:

1. **Stage 1 — resolve_primitive**: Replace `is_local_agent_tool()` / `is_vision_datasource_tool()` / `is_table_datasource_tool()` with a unified `resolve_primitive(tool_name) -> PrimitiveKind` that returns a routing decision struct.

2. **Stage 2 — prepare_inputs**: Extract and normalize inputs based on `PrimitiveKind`. For `image.*`: fetch URL or decode base64. For `audio.*`: fetch and transcode. For `text.*`: pass through.

3. **Stage 3 — merge_context**: Build the `CapabilityRequest` with the current conversation identity, agent ID, and rights envelope. This replaces the per-tool context injection scattered across the current routing code.

4. **Stage 4–6 dispatch**: Send `IpcRequest::CapabilityRequest` and stream results back — same path for all primitives.

---

### Slice 9 — First Multi-Primitive Model-Controller (ONNX)
**Status**: Planned | Est: 2 days

Merge `model-controller-onnx-* ` binaries into a single `model-controller-onnx` that implements `CapabilityProvider`:

```
model-controller-onnx
  capabilities: [
    "text.embed",          ← EmbeddingsBackend
    "audio.transcribe",    ← WhisperBackend
    "voice.synthesize",    ← KokoroBackend
    "image.ocr",           ← VisionBackend (Florence-2)
    "image.ground",        ← VisionBackend (Florence-2)
  ]
```

Single binary, single `ModelProfileRecord`, single `run_capability_controller` runtime. Proves the merge works before touching Gemini or OpenAI.

Remove `model-controller-vision` (now redundant).

---

### Slice 10 — Gemini Capability Provider
**Status**: Planned | Est: 2 days

Extend `model-controller-gemini` to implement `CapabilityProvider`:

```
model-controller-gemini
  capabilities: [
    "text.generate",       ← existing
    "response.generate",   ← existing
    "image.describe",      ← Gemini Vision (NEW)
    "image.ocr",           ← Gemini Vision (NEW, cloud fallback)
    "audio.transcribe",    ← Gemini Audio (NEW, cloud fallback)
  ]
```

Gemini becomes the cloud fallback for ONNX-local capabilities. When `model-controller-onnx` is unhealthy or missing a capability, the routing reflex falls back to Gemini automatically.

---

### Slice 11 — Deprecate Old Interfaces
**Status**: Planned | Est: 1 day

Once all model-controllers use `CapabilityProvider`:

- Remove `ModelProvider` trait and `run_model_controller` runtime
- Remove `DatasourceProvider` trait and `run_datasource_controller` runtime
- Remove `is_vision_datasource_tool`, `is_table_datasource_tool`, etc. from philote
- Remove `vision.setup` / `vision.status` IPC handlers (no longer needed — ONNX loads automatically)
- Remove `model-controller-vision` binary reference from `justfile` and Cargo workspace

---

## Binary Map

### Current
```
model-router binary (model path)
  └── OnnxProvider:  [text.embed, audio.transcribe, voice.synthesize]
  └── GeminiProvider: [text.generate, response.generate, media.analyze]
  └── ElevenLabsProvider: [voice.synthesize]

model-controller-vision binary (datasource path)
  └── OnnxVisionProvider: [image.ocr, image.ground]    ← Slice 5 (just shipped)

model-controller-graph-datasource binary (datasource path)
  └── GraphProvider: [graph.query, graph.create, ...]

model-controller-table-datasource binary (datasource path)
  └── TableProvider: [table.query, table.insert, ...]
```

### Target (post Slice 11)
```
model-controller-onnx binary (capability path)
  └── [text.embed, audio.transcribe, voice.synthesize, image.ocr, image.ground]

model-controller-gemini binary (capability path)
  └── [text.generate, response.generate, image.describe, image.ocr, audio.transcribe]

model-controller-openai binary (capability path)
  └── [text.generate, image.describe, image.ocr]

model-controller-graph-datasource binary (capability path)
  └── [graph.query, graph.create, ...]

model-controller-table-datasource binary (capability path)
  └── [table.query, table.insert, ...]
```

All routing via `ModelProfileRecord.task_kinds`. No hardcoded `target_role` strings in philote.

---

## Open Questions

1. **Streaming for table/graph datasource**: These are fundamentally non-streaming (query → result set). The unified streaming protocol accommodates this via `result` + `done` events, but the overhead of the event protocol may not be worth it. Consider keeping them on a separate simplified path or emit streaming for large result sets.

2. **Vision.setup / vision.status**: Currently these IPC handlers configure the Python backend. With ONNX, they become less useful — the model auto-downloads on first use. Either repurpose them (model cache warmup, backend status inspection) or remove them in Slice 11.

3. **Stage 2 prepare_inputs in philote vs hotel**: Image resizing and audio transcoding could move to the hotel so all agents share a single prep pipeline. Deferred — get the hotel router working first, then evaluate.

4. **KV-cache for decoder inference**: Current `VisionBackend` uses O(n²) greedy decode (same as `WhisperBackend`). Add merged-decoder KV-cache in a follow-up once the routing architecture is stable.
