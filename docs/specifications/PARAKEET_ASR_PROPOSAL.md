# Parakeet ASR Integration Proposal

**Status**: Draft
**Scope**: Replace or supplement Whisper with NVIDIA Parakeet for on-device ASR
**Date**: 2026-04-29

---

## Background

The Philotic Stack has two ASR paths:
- `crates/onnx-runner` — ONNX Whisper (encoder + greedy decoder) via `WhisperBackend` at `backends/transcribe.rs`
- `crates/mlx-runner` — MLX Whisper on Apple Silicon, shelling out to `mlx_whisper` via `MlxWhisperHandle`

Both write `WhisperTrainingSample` records to SQLite via `WhisperTrainingStorage` in `ansible-mesh-core`. The `router-listener` guest captures voice turns; `phil training export` feeds the fine-tuning pipeline.

---

## Model Selection

### Available Models (NVIDIA NeMo, Apache 2.0)

| Model | Params | Architecture | WER (LibriSpeech clean) | Notes |
|---|---|---|---|---|
| `parakeet-tdt-0.6b-v2` | 600M | TDT (Token-and-Duration Transducer) | ~1.7% | Best accuracy/size tradeoff; streaming-capable |
| `parakeet-ctc-1.1b-v2` | 1.1B | CTC | ~1.4% | Highest accuracy; no streaming |
| `parakeet-ctc-0.6b-v2` | 600M | CTC | ~1.6% | Simpler decoder |

Whisper large-v3 benchmarks ~2.7% WER on the same set. Parakeet TDT 0.6b-v2 cuts that roughly in half at the same parameter count.

**Recommendation**: `parakeet-tdt-0.6b-v2` as primary. `parakeet-ctc-1.1b-v2` as optional high-accuracy variant on the Linux VPS where memory headroom is larger.

---

## Execution Paths

### Apple Silicon (MacBook Air — primary dev node)

MLX has no Parakeet port yet. NeMo uses PyTorch MPS on Apple Silicon automatically.

1. **Python subprocess (Slice 1, immediate)** — same pattern as `MlxWhisperHandle`. One-shot NeMo Python script per transcription. ~300–600ms for 5-second audio on M-series with MPS. Works today.

2. **ONNX export (Slice 2)** — NeMo has first-class ONNX export (`model.export("model.onnx")`). CTC exports cleanly (single encoder + CTC head). TDT needs opset ≥ 17 (more complex). ONNX Runtime on Apple Silicon uses the CoreML execution provider. Fits `onnx-runner` exactly.

### Linux VPS (jane-vps)

No GPU. CPU ONNX via ORT is ~2× faster than native PyTorch CPU. ONNX only; skip NeMo subprocess on VPS.

---

## Architecture in Philotic Stack

### Slice 1: New `parakeet-runner` crate (subprocess path)

Mirrors `mlx-runner`:
- `ParakeetHandle` wraps `nemo_asr` Python subprocess
- HTTP sidecar (`POST /transcribe`) returning same JSON shape as other runners
- Capability advertisement: `asr/parakeet-tdt-0.6b-v2`
- `model-router` selects Parakeet over Whisper when advertised
- `just start-parakeet` justfile target

### Slice 2: New `onnx-runner/src/backends/parakeet.rs`

Production path:
- `ParakeetCTCBackend` alongside existing `WhisperBackend`
- New `ParakeetHandle` in `onnx-runner/src/hub.rs`
- CTC variant first (clean ONNX export); TDT after export validation
- Benchmark gate: must match or beat mlx_whisper latency on Apple Silicon

Both coexist; `model-router` picks backend by capability advertisement at runtime.

---

## Training Data Pipeline

`WhisperTrainingSample` is model-agnostic — `model_gen` is a free-form string, `audio_path` stores raw WAV, `corrected_transcript` stores ground truth. No schema migration needed.

NeMo fine-tuning consumes a manifest JSON (`{"audio_filepath": ..., "text": ..., "duration": ...}` per line). The existing storage maps 1:1.

**Addition needed**: `--format nemo` flag on `phil training export` emits NeMo manifest format instead of HuggingFace dataset format.

Speaker adaptation: NeMo supports LoRA fine-tuning on CTC/TDT heads with ~30–60 minutes of corrected audio. The `router-listener` capture loop plus `/correct` operator commands feeds this pipeline without structural changes.

---

## Implementation Slices

**Slice 1 — Subprocess runner (2–3 days)**
- `crates/parakeet-runner` with `ParakeetHandle` and HTTP sidecar
- Capability advertisement wired into `model-router`
- `just start-parakeet` target
- Smoke test: bjork voice turn transcribed via Parakeet, stored in `WhisperTrainingStorage`

**Slice 2 — ONNX backend (3–4 days)**
- `scripts/export-parakeet-onnx.py` (NeMo → ONNX, CTC variant)
- `crates/onnx-runner/src/backends/parakeet.rs` — `ParakeetCTCBackend`
- Hub entry in `onnx-runner/src/hub.rs`
- Benchmark gate vs mlx_whisper latency

**Slice 3 — Fine-tuning pipeline (2–3 days)**
- `phil training export --format nemo` subcommand
- `scripts/finetune-parakeet.py` (NeMo LoRA fine-tune)
- `model_gen` tracking for Parakeet checkpoints in `WhisperTrainingSample`

---

## Key Risks

1. **TDT ONNX export**: The duration predictor uses dynamic control flow; needs opset ≥ 17 and ORT version testing. Start with CTC, add TDT after validation.

2. **NeMo install size**: `nemo_toolkit[asr]` is ~4 GB. Needs a managed venv — same unsolved problem as `mlx-lm` in mlx-runner. A shared `philotic-python-env` setup script is the right solution.

3. **VPS memory ceiling**: parakeet-tdt-0.6b-v2 in fp16 is ~1.2 GB RAM. VPS machines with <4 GB may need int8 quantization (NeMo supports this; ONNX int8 path needs validation separately).

4. **Conversational WER gap**: Parakeet benchmarks are on read speech (LibriSpeech). Bjork's conversational voice turns may narrow the gap vs Whisper. Run empirical evaluation on captured samples before declaring a win. The existing training DB is the right source for this.

---

## Summary Recommendation

Ship Slice 1 (subprocess runner) first to get real-world WER data on bjork's voice. If WER improves materially (target: <3% vs Whisper-small's ~5–7% on conversational speech), proceed to Slice 2. The training pipeline (Slice 3) is independent and can run in parallel. No existing crate needs breaking changes.
