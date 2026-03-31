---
title: MLX Model Controller
doc_type: proposal
domain: tooling-execution
status: proposed
last_updated: 2026-03-31
tags:
- mlx
- local-inference
- model-controller
- model-router
- apple-silicon
related_docs:
- MODEL_CONTROLLER_PROPOSAL.md
- LOCAL_ONNX_INFERENCE_PROPOSAL.md
- ARCHITECTURE_STATUS.md
- SEAM_REGISTRY.md
task_refs:
- docs/task.md
proposal_id: mlx-model-controller
active_seams:
- mlx-runner-fleet
- mlx-provider-dispatch
- model-role-naming-migration
---

# MLX Model Controller

## Goal

Provide a local, Apple Silicon-accelerated inference surface for the Philotic Stack using
[MLX](https://github.com/ml-explore/mlx) / [mlx-lm](https://github.com/ml-explore/mlx-examples/tree/main/llms).

The controller manages a **fleet** of `mlx_lm.server` instances — one per model — and
presents them as a single `ModelProvider` to the existing `ProviderRegistry`. It handles
health tracking, class-based dispatch, priority-ordered fallback, and startup discovery.

---

## Role Naming: `model.<impl>` (No Locality Tier)

**Decision**: Role names identify the *controller implementation*, not its locality.
Locality (`local` vs `remote`) is a routing **policy** concern, expressed as a
preference ordering at the agent or hotel profile level — not baked into the role
identity string.

| Role                  | Controller            |
|-----------------------|-----------------------|
| `model`               | broker / any          |
| `model.mlx`           | this controller       |
| `model.onnx`          | ONNX controller       |
| `model.gemini`        | Gemini                |
| `model.elevenlabs`    | ElevenLabs            |

Routing preference (e.g., "prefer local models for this agent") is expressed as an
ordered list in hotel/agent config: `["model.mlx", "model.gemini"]`. The
`ProviderRegistry` already supports ordered fallback via `resolve()`.

**Migration**: `model.local` → `model.onnx`. ONNX controller subscribes to both
during a transition window, then drops `model.local`.

---

## Core Recommendation

### `mlx-runner` — library crate

A library crate containing:
- `MlxModelConfig` — per-instance config (repo, port, mode, class, priority)
- `MlxModelInstance` — health state machine + HTTP dispatch to one `mlx_lm.server`
- `MlxServerHandle` — subprocess lifecycle for managed instances
- `MlxClient` — OpenAI-compatible HTTP client + tool call parsing
- `MlxWhisperHandle` — `mlx_whisper` subprocess wrapper for transcription

No `main.rs`, no IPC registration. Plugs into `model-router` via `MlxProvider`.

### `MlxProvider` in `model-router`

Implements `ModelProvider`. Owns the fleet:

```
MlxProvider
├── text_models:       Vec<MlxModelInstance>   (mlx_lm.server)
├── multimodal_models: Vec<MlxModelInstance>   (mlx_vlm — deferred, see below)
└── transcribe_handles: Vec<MlxWhisperHandle>  (mlx_whisper subprocess)
```

`supports(task)` returns `true` only if at least one `Healthy` instance exists for that
task class. This makes `ProviderRegistry` fallback automatic: if all MLX text models are
down, Gemini is tried next.

`invoke(task)` selects the highest-priority healthy instance in the relevant class,
dispatches, and marks the instance `Degraded` on failure so subsequent requests in the
same tick are rerouted.

### `model-controller-mlx` binary

Same shape as `model-controller-onnx`. Reads fleet config from hotel config store,
runs startup health checks, registers as role `model.mlx`, enters IPC loop via
`run_model_controller()`.

---

## Fleet Configuration

Stored in hotel config (not env vars — the fleet is multi-model). Shape:

```json
{
  "mlx_controller": {
    "health_check_interval_secs": 300,
    "models": [
      {
        "class": "text",
        "repo_id": "mlx-community/Qwen2.5-72B-Instruct-4bit",
        "mode": "managed",
        "port": 11441,
        "priority": 100,
        "extra_args": []
      },
      {
        "class": "text",
        "repo_id": "mlx-community/Llama-3.2-3B-Instruct-4bit",
        "mode": "managed",
        "port": 11442,
        "priority": 50
      },
      {
        "class": "text",
        "repo_id": "mlx-community/some-running-model",
        "mode": "attached",
        "host": "localhost",
        "port": 11450
      },
      {
        "class": "transcribe",
        "repo_id": "mlx-community/whisper-large-v3",
        "priority": 100
      }
    ]
  }
}
```

`mode` is either `"managed"` (controller spawns and owns the process) or `"attached"`
(controller connects to an already-running server). `extra_args` passes flags directly
to `mlx_lm.server` (quantization, context length, etc.) and is treated as opaque — no
Philotic-specific interpretation.

Per-request overrides arrive via `provider_options` (e.g.,
`{ "prefer_model": "mlx-community/Llama-3.2-3B-Instruct-4bit" }` to pin to a specific
instance for a turn).

---

## Connection Modes

### Managed
1. Controller spawns `python -m mlx_lm.server --model <repo> --port <port> [extra_args]`
2. Polls `GET /v1/models` with exponential backoff until ready (timeout: 60s)
3. Verifies the loaded model matches `repo_id` in config (trust but verify)
4. If mismatch: log warning, mark `Degraded` — still serve, it may be compatible

### Attached
1. On startup: `GET /v1/models` → discover what model is actually loaded
2. Record discovered model in instance state (may differ from `repo_id` in config)
3. Proceed as `Healthy` if reachable, regardless of model match

In both modes, the discovered model identity is recorded in `RouterTraceStorage`
records (via the `trace.model` field) so the RL flywheel sees what actually served.

---

## Health Check Strategy

```
Startup (parallel across all instances):
  Managed  → spawn → poll /v1/models → verify → Healthy or Degraded
  Attached → GET /v1/models → record model → Healthy or Down

Periodic background ticker (default: 5 min):
  GET /v1/models per instance
  success   → reset to Healthy (clears degraded failure count)
  failure   → increment failure_count
              ≥2 failures → Degraded
              ≥5 failures → Down

On request failure (fail-fast, no retry on same instance):
  immediate → mark Degraded
  reroute to next healthy instance in class
  no sleep, no backoff on the request path

Down → recovery:
  periodic tick confirms reachable → reset to Healthy
```

Health checks never block the request path. The periodic check is background state
maintenance only.

---

## Selection & Fallback

Within a class (e.g., `text`), on each request:

1. Filter: instances where `health == Healthy`
2. Check `provider_options.prefer_model`: if set and matches a healthy instance, use it
3. Otherwise: pick highest `priority` (higher number = bigger model = preferred)
4. On invoke failure: mark `Degraded`, pick next in priority order
5. All instances down: `supports()` returns `false` → `ProviderRegistry` tries next
   provider (e.g., `model.gemini`)

Cross-class degraded path:
- `MediaAnalyze` with no healthy multimodal → fail with `backend_unavailable`
  (no silent text-only fallback — degraded multimodal is caller's decision)
- `AudioTranscribe` with no whisper handle → `backend_unavailable`

---

## Priority: Bigger Model Default

When no `provider_options.prefer_model` is set, the controller always routes to the
highest-priority (numerically largest) healthy instance. Convention:

- Priority 100+ → primary/large model (e.g., 72B)
- Priority 50   → secondary/small model (e.g., 3B)
- Priority 10   → tertiary / attached external

This matches the user expectation: "prefer the bigger model" without requiring
per-request hints.

---

## Task Class Coverage

| Class       | Backend              | Phase   | Notes                            |
|-------------|----------------------|---------|----------------------------------|
| TextGenerate | mlx_lm.server        | Phase 1 | OpenAI chat completions API      |
| Tool calling | mlx_lm.server        | Phase 1 | model-dependent (Qwen, Llama-3)  |
| AudioTranscribe | mlx_whisper subprocess | Phase 2 | JSON output, audio file temp path |
| MediaAnalyze | mlx_vlm.server       | Deferred | See VLM note below              |
| VoiceSynthesize | —                 | Out of scope | Stays with ElevenLabs        |

---

## VLM (Multimodal) — Deferred but Designed In

`mlx_vlm` has a different server interface from `mlx_lm.server`. To accommodate it
without structural changes later:

- `MlxModelConfig` includes a `server_variant: MlxServerVariant` field
  (`MlxLm` | `MlxVlm`)
- `MlxModelInstance::chat()` dispatches to the correct request/response format
  based on `server_variant`
- `multimodal_models` class exists in `MlxProvider` — just empty at Phase 1

When `mlx_vlm` is needed (e.g., for Anj's use case), adding a multimodal entry to
the fleet config is sufficient — no structural changes to the provider or crate.

---

## Crate Structure

```
crates/mlx-runner/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── config.rs        # MlxModelConfig, MlxMode, MlxServerVariant, fleet config
    ├── instance.rs      # MlxModelInstance: health state machine, invoke dispatch
    ├── server.rs        # MlxServerHandle: subprocess spawn/supervise (managed mode)
    ├── client.rs        # MlxClient: OpenAI-compat HTTP, tool call parsing, streaming
    └── whisper.rs       # MlxWhisperHandle: mlx_whisper subprocess, audio → text

crates/model-router/
└── src/
    ├── providers/
    │   └── mlx.rs       # MlxProvider: fleet orchestration, class dispatch
    └── bin/
        └── model-controller-mlx.rs  # guest binary: load config, start fleet, IPC loop
```

---

## Seams

| Seam ID                      | Description                                           | Status   |
|------------------------------|-------------------------------------------------------|----------|
| `mlx-runner-fleet`           | `mlx-runner` crate: config, instance, server, client | Proposed |
| `mlx-provider-dispatch`      | `MlxProvider` in model-router                        | Proposed |
| `mlx-controller-binary`      | `model-controller-mlx` guest binary                  | Proposed |
| `model-role-naming-migration`| Rename `model.local` → `model.onnx`                  | Proposed |

---

## Open Threads

- **Streaming partial_replies**: `mlx_lm.server` supports SSE. SSE → `partial_replies`
  in `ProviderOutput::Text` is Phase 3 work.
- **RL flywheel tap**: The `RouterTraceStorage` tap already exists (Seam 5, B1).
  MLX controller wires into it via the same `PHILOTIC_ROUTER_TRACE_DB` path.
- **`mlx_lm` availability check**: Binary fails fast at startup if `mlx_lm` is not
  importable (`python -c "import mlx_lm"` check before spawning managed instances).
