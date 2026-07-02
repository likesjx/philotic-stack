---
title: Local ONNX Inference Runner
doc_type: proposal
domain: memory-context
status: proposed
disposition: implemented
last_updated: 2026-03-31
tags:
- onnx
- embeddings
- local-inference
- model-router
- muninn
- rl-training
related_docs:
- MODEL_CONTROLLER_PROPOSAL.md
- VOICE_MACHINE_PROPOSAL.md
- MUNINN_MEMORY_PROTOCOL_PROPOSAL.md
- ARCHITECTURE_STATUS.md
task_refs:
- docs/task.md
proposal_id: local-onnx-inference
active_seams:
- onnx-runner-embed-surface
- model-router-embed-kind
- muninn-embed-integration
---

# Local ONNX Inference Runner

## Goal

Provide a first-class local inference surface for the Philotic Stack with three
capability backends — embeddings, local function/generative inference, and speech
transcription — using ONNX Runtime as the execution engine, HuggingFace Hub for
model management, and an Ollama-compatible HTTP sidecar so existing clients
(Muninn, vector stores, tooling) can talk to it without modification.

`onnx-runner` is a **model-router provider** — it plugs into the existing
`ModelProvider` trait and `ProviderRegistry` just like `GeminiProvider` and
`ElevenLabsProvider`. The IPC routing path is the primary surface; the Ollama-
compatible HTTP sidecar is an additional surface on the same process, serving
the same backends.

The primary motivation is the RL training loop: as models are fine-tuned and
swapped, the embedding semantic space changes. The runner must be the single
authority on the current model generation so downstream consumers can detect
and respond to space drift.

---

## Core Recommendation

### `onnx-runner` — a lib crate, not a guest binary

`onnx-runner` is a **library crate** containing:
- Three ONNX inference backends (`EmbeddingsOnnx`, `FunctionOnnx`, `TranscribeOnnx`)
- HuggingFace Hub model management (pull, cache, hot-swap)
- No `main.rs`, no IPC registration — it has no opinions about the surface

### `OnnxProvider` in model-router

`model-router/src/providers/onnx.rs` implements `ModelProvider` by wrapping the
`onnx-runner` lib. It declares support for `TaskKind::Embed`,
`TaskKind::TextGenerate`, `TaskKind::AudioTranscribe` and dispatches to the
appropriate backend. It is registered in `ProviderRegistry` like any other provider.

### `model-controller-onnx` binary

`crates/model-router/src/bin/model-controller-onnx.rs` is the entry point:

```rust
tokio::join!(
    run_model_controller(ipc_config),  // IPC routing path (existing pattern)
    run_onnx_sidecar(sidecar_config),  // Ollama-compat HTTP on 11435
)
```

Both surfaces run concurrently in the same process, sharing the same backend
instances. The sidecar is an additive surface — callers that speak Ollama
(Muninn, vector tools) hit port 11435; callers that go through the hotel routing
plane hit IPC.

### Dependency graph

```
crates/onnx-runner          (lib: backends + HF Hub)
    ↑
crates/model-router         (OnnxProvider wraps onnx-runner)
    ↑
src/bin/model-controller-onnx.rs   (IPC loop + sidecar, tokio::join!)

crates/training-collector   (depends on onnx-runner lib directly)
crates/philote              (optional: in-process embed without IPC/HTTP)
```

### Model-router extension

Add `TaskKind::Embed` to model-router so the IPC routing path works end-to-end.
`RequestClass::Embedding` currently bails — this unblocks it.

---

## Disposition

`proposed` — alignment confirmed, no implementation started.

---

## Architecture

### Dual Surface

```
Muninn / VectorDB / tools
         │
         ▼
  http://localhost:11435    ← Ollama-compat HTTP sidecar
         │                     (spawned by model-controller-onnx binary)
         ▼
  OnnxProvider / onnx-runner backends
         ▲
  IPC /tmp/philotic-{hotel}.sock
         │
  ProviderRegistry::resolve() in model-controller-onnx
         │
  Philote / model-router routing plane
```

The HTTP sidecar and the IPC routing path share the same `OnnxProvider` instance
in the same process — same backends, same model state, same `model_gen` token.
The sidecar is the fast path for latency-sensitive consumers (Muninn recall,
vector index updates). The IPC path is the routing path for agent-driven tasks.

Proxy fallback: if the requested model is not loaded in `OnnxProvider`, the
sidecar can forward to `localhost:11434` (real Ollama). The IPC path does not
proxy — it fails with a clear provider-not-available error.

### Three Backends

| Backend | Capability | Task kind | Default model | Notes |
|---|---|---|---|---|
| `EmbeddingsOnnx` | `text.embed` | `TaskKind::Embed` | `onnx-community/embeddinggemma-300m-ONNX` | Google's dedicated 302.9M embedding model, Gemma 3 architecture, ONNX export by `onnx-community` |
| `FunctionOnnx` | `text.generate` | `TaskKind::TextGenerate` | `onnx-community/gemma-3-1b-it-ONNX` | Instruction-tuned Gemma 3 1B, local generative inference |
| `TranscribeOnnx` | `voice.transcribe` | `TaskKind::AudioTranscribe` | `onnx-community/whisper-small` | Whisper ONNX with CoreML EP on macOS |

**EmbeddingGemma note**: `google/embeddinggemma-300m` is gated (Gemma license). The
`onnx-community/embeddinggemma-300m-ONNX` export is the correct pull target — it is
accessible without individual gating and carries quantized variants. For RL fine-tuning,
the plan is to fine-tune from the base weights and export a custom ONNX revision, then
pull that into `onnx-runner` via the model swap mechanism. The base model repo and
revision are tracked as the `model_gen` origin.

Models are identified by a config key (e.g. `embed_model`, `function_model`,
`transcribe_model`) pointing to a HuggingFace repo+revision. Each backend holds
the loaded ONNX session and exposes `infer()`. The backend name is conceptual
and stable; the loaded model is swappable at runtime.

**WhisperKit note**: `TranscribeOnnx` implements the same semantics as Apple's
WhisperKit — Whisper inference with CoreML execution provider on macOS/Apple
Silicon. The capability is identical; the Rust/ONNX stack avoids Swift/FFI
while getting the same hardware acceleration path via ONNX Runtime's CoreML EP.

### ONNX Runtime crate: `ort` (v2.x)

Official Rust bindings for ONNX Runtime. Execution providers relevant here:
- **CoreML EP** — Apple Silicon acceleration (macOS, enabled by default when available)
- **CPU EP** — universal fallback

### HuggingFace model management: `hf-hub`

```
onnx-runner pull --backend embed --repo BAAI/bge-small-en-v1.5 --revision main
onnx-runner pull --backend transcribe --repo openai/whisper-base --revision main
```

Downloaded to a local cache dir (e.g. `~/.cache/philotic/onnx-models/`). Model
generation is tracked by `(repo, revision, sha)`. Hot-swap: download new model,
atomic pointer swap in the backend, emit `model.swapped` event to the hotel.

### Model Generation Token

Every embedding output carries a `model_gen` field: `"{repo}@{sha8}"`.

This allows consumers to:
- Detect when the embedding space has shifted
- Invalidate or re-embed stale vectors in the index
- Muninn: tag engrams with `model_gen`, trigger re-enrichment when generation changes

---

## RL Training Flywheel

The long-term goal is a fully autonomous closed-loop RL pipeline. `onnx-runner`
is the execution node; the training flywheel wraps it.

### Pipeline Overview

```
Live inference
     │
     ▼
training-collector guest          ← router listener, post-turn hook
     │  (input, output, reward)
     ▼
HuggingFace dataset repo          ← likesjx/philotic-{backend}-training-data
     │
     ▼
RL training  (HF AutoTrain / HF Space / GH Actions + Accelerate)
     │
     ▼
HuggingFace model repo            ← likesjx/embeddinggemma-300m-philotic (staging slot)
     │
     ▼  (promotion gate)
production slot                   ← model-registry marks as production
     │
     ▼
onnx-runner pull + hot-swap       → model.swapped IPC → Muninn re-embed
```

### Training Data Collection

A `training-collector` guest subscribes as a post-turn router listener. It does
not modify the routing path — it observes only. Per backend:

| Backend | Training objective | Captured data shape |
|---|---|---|
| `EmbeddingsOnnx` | Contrastive metric learning | `(anchor, positive, negative)` triplets derived from session co-occurrence and recall outcomes |
| `FunctionOnnx` | DPO / preference | `(context, chosen_action, rejected_action)` from approval/deny signals |
| `TranscribeOnnx` | Supervised fine-tune | `(audio_ref, transcript, correction)` when corrections exist |

**Reward signal for embeddings (open design question)**: The reward cannot be
evaluated on the vector alone — it requires a downstream signal. Candidate
sources: recall quality (did the retrieved context lead to a successful turn?),
agent correction events, explicit user feedback. This must be resolved before
the collector schema is finalized for `EmbeddingsOnnx`.

### HuggingFace Dataset Push

`training-collector` buffers examples locally and flushes to a private HF
dataset repo on a schedule (count-based or time-based). Format: Parquet shards,
schema versioned by `model_gen` of the producing model so training runs know
which embedding space the data was produced in.

Requires: HF token with dataset write access, stored in the hotel key vault.

### Training Trigger

Options (decision deferred):
- **HF AutoTrain**: simplest, least control
- **Custom HF Space**: full control over training loop, supports RL objectives
- **GitHub Actions + Accelerate**: most portable, GPU runner cost

Trigger: webhook from dataset repo on new shard, or manual `onnx-runner train`
IPC command.

### Model Registry

Two slots per backend: `staging` and `production`.

- Training runs push to `staging` (e.g. `likesjx/embeddinggemma-300m-philotic@sha`)
- A promotion step (manual or automated eval gate) moves `staging` → `production`
- `onnx-runner` only auto-pulls `production` slot on schedule; `staging` requires
  explicit pull for testing
- Registry state lives in the aiua context graph (a `model_registry` node per backend)

This prevents a regressing training run from auto-promoting to live inference.

### Seam: Embedding Space Drift on Swap

When a fine-tuned model replaces the current one, the semantic space shifts.
All vectors in Muninn and any vector index are in the old space.

**Invariant**: `onnx-runner` emits `model.swapped` IPC event on every hot-swap.
Every embedding output carries `model_gen: "{repo}@{sha8}"` from day one.
Muninn tags engrams with `model_gen` and triggers re-enrichment when generation
changes. Active seam: `onnx-runner-embed-surface`.

This is not implemented in Slice 1 — the output format carries it from the start.

---

## Ollama-Compatible HTTP Surface

Port `11435` — leaves Ollama's default `11434` free for optional proxy fallback.

Endpoints implemented:
```
POST /api/embeddings          ← EmbeddingsOnnx
POST /api/generate            ← FunctionOnnx (non-streaming first, streaming later)
POST /api/transcribe          ← TranscribeOnnx (non-standard but Ollama-adjacent)
GET  /api/tags                ← list loaded models
POST /api/pull                ← trigger HF Hub download + load
```

Muninn configuration: point `ollama_base_url` at `http://localhost:11435`.

---

## Guest Registration

`model-controller-onnx` registers with the hotel as a standard model controller
guest (e.g. `model-onnx-01`) with role `model.local`. It uses the existing
`run_model_controller` runtime with `OnnxProvider` registered in the provider
list. The hotel routes `text.embed`, `text.generate`, and `voice.transcribe`
tasks to it via the normal capability resolution path — no special-casing.

This is identical in shape to `model-controller-gemini` and
`model-controller-elevenlabs`; the only difference is the binary also runs the
HTTP sidecar concurrently.

---

## Model-Router Changes Required

1. Add `TaskKind::Embed` (capability string: `"text.embed"`)
2. Add `ProviderOutput::Embedding { vector: Vec<f32>, model_gen: String }`
3. Add `ControllerResponseEnvelope` branch for `Embedding` output (artifact kind `"embedding"`)
4. Remove `RequestClass::Embedding` bail
5. No new provider in model-router itself — routing resolves to `onnx-runner` guest via IPC

---

## Slice Sequence

### Slice 1 — `onnx-runner` foundation + embeddings surface (first ship)
- `crates/onnx-runner` crate skeleton
- `ort` + `hf-hub` dependencies
- `EmbeddingsOnnx` backend: load model, run inference, return `Vec<f32>`
- Ollama-compat `POST /api/embeddings` live on `11435`
- HF Hub pull + local cache working
- `model_gen` token in every output
- Philotic guest IPC registration with `text.embed` capability
- `TaskKind::Embed` + `ProviderOutput::Embedding` in model-router

### Slice 2 — `TranscribeOnnx` (WhisperKit semantics)
- Whisper ONNX model loaded via `hf-hub`
- `voice.transcribe` via IPC + `/api/transcribe` via HTTP
- CoreML EP enabled on macOS
- Replaces/supplements Gemini transcription path

### Slice 3 — `FunctionOnnx` (local generative)
- Gemma-2B-it or Phi-3-mini ONNX
- `text.generate` via IPC + `/api/generate` via HTTP
- Non-streaming first; streaming deferred
- Config-gated (larger memory footprint)

### Slice 4 — Ollama proxy fallback
- If requested model not loaded, forward to `localhost:11434`
- Transparent to callers

### Slice 5 — Hot-swap + RL model_gen invalidation
- `onnx-runner pull` swaps model atomically at runtime (HF Hub + local path)
- `model.swapped` IPC event emitted
- Muninn re-embedding signal path
- Model registry node in aiua context graph (`staging` / `production` slots)

### Slice 6 — Training flywheel
- `training-collector` guest crate (post-turn router listener)
- Per-backend collector schemas (embed triplets, DPO pairs, transcription pairs)
- HF Hub dataset flush (Parquet shards with `model_gen` provenance)
- Training trigger integration (HF AutoTrain or custom Space)
- Auto-pull from `production` slot on new model version
- Promotion gate (manual first, eval-automated later)

---

## Current Slice

**Slice 1** — not started.

---

## Open Questions

- **EmbeddingGemma embedding dimension**: Gemma 3 300M hidden size is 1152. Need to
  verify the ONNX export output shape (likely `[batch, seq, 1152]` with mean pooling
  to `[batch, 1152]`). Confirm during Slice 1 model loading.
- **`ort` dynamic vs static linking**: Static is simpler for distribution; dynamic
  required if we want to swap ONNX Runtime versions. Recommend static for Slice 1,
  revisit at distribution time.
- **Muninn integration path**: Muninn's `muninn_remember` / `muninn_recall` go through
  its own embedding pipeline. Does Muninn call our HTTP surface directly (Ollama-compat
  on `11435`), or does the Philotic stack inject embeddings into Muninn via its
  enrichment hook? This determines whether a config change is enough, or if there's
  a protocol extension needed. Likely the HTTP surface is sufficient for Slice 1.
- **FunctionOnnx default model**: `onnx-community/gemma-3-1b-it-ONNX` is a placeholder
  — confirm ONNX availability and quantized variant before Slice 3 starts.
- **RL fine-tune export workflow**: ONNX export step must be part of the training
  pipeline (e.g. `optimum-cli export onnx` after training). The hot-swap mechanism
  needs a local-path pull option alongside HF Hub for in-the-loop testing.
- **Reward signal for EmbeddingsOnnx**: Must be resolved before Slice 6 collector
  schema is finalized. Candidates: recall quality from session outcome tracking,
  contrastive from agent co-occurrence, explicit correction signals.
- **Training platform**: HF AutoTrain vs. custom HF Space vs. GH Actions + Accelerate.
  Decision gates Slice 6 trigger design.
- **`onnx-runner` lib+bin split**: Structure as a lib + bin crate so other workspace
  members (philote, training-collector) can depend on inference functionality directly
  without going through IPC/HTTP. HTTP and IPC become thin wrappers over the lib.
  Confirm before Slice 1 crate scaffold.
