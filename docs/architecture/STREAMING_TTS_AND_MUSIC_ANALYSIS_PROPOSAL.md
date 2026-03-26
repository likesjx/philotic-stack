---
title: "ElevenLabs Streaming TTS and ONNX Music Analysis"
doc_type: proposal
domain: tooling-execution
status: proposed
last_updated: 2026-03-24
tags:
  - elevenlabs
  - tts
  - streaming
  - onnx
  - music
  - midi
  - model-router
  - local-inference
related_docs:
  - LOCAL_ONNX_INFERENCE_PROPOSAL.md
  - VOICE_MACHINE_PROPOSAL.md
  - MODEL_CONTROLLER_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: streaming-tts-and-music-analysis
active_seams:
  - elevenlabs-streaming-tts
  - elevenlabs-stt-surface
  - onnx-music-analysis-surface
  - midi-output-artifact
---

# ElevenLabs Streaming TTS and ONNX Music Analysis

## Goal

Extend the model-router with two complementary capabilities:

1. **ElevenLabs Streaming TTS** — replace the current full-buffer TTS path with a
   chunked streaming path, eliminating the full-audio-wait latency before playback
   begins. This requires a new IPC delivery mechanism for partial audio artifacts.

2. **ONNX Music Analysis** — add a `MusicAnalysisBackend` to the `onnx-runner` lib
   crate, enabling local piano/organ transcription and music structure analysis. Output
   is MIDI bytes or structured notation rather than text, requiring a new `ProviderOutput`
   variant.

These two capabilities are pinned in the same proposal because they share a common
infrastructure requirement: both need a richer output type beyond the existing
`Text`, `Embedding`, and `Audio` variants — streaming audio chunks and MIDI/notation
artifacts respectively. Designing the output type extension once for both avoids
two incompatible widening PRs.

---

## Context

### What exists today

- `ElevenLabsProvider` in `model-router` implements `VoiceSynthesize` via
  `POST /v1/text-to-speech/{voice_id}`. The full MP3 buffer is collected before
  returning `ProviderOutput::Audio(AudioArtifact { ... })`.
- `OnnxProvider` implements `Embed` (`EmbeddingsBackend`) and `AudioTranscribe`
  (`WhisperBackend`).
- `ProviderOutput` has variants: `Text { ... }`, `Embedding { vector, model_gen }`,
  `Audio(AudioArtifact)`.
- `TaskKind` has: `TextGenerate`, `Embed`, `AudioTranscribe`, `VoiceSynthesize`,
  `MediaAnalyze`.

### What is missing

- No chunked/streaming path through IPC or the provider trait.
- No MIDI/notation output type — `AudioArtifact` carries a blob URI, not structured
  musical data.
- No `MusicAnalyze` task kind.
- No ElevenLabs STT surface (deferred from ONNX Slice 2 discussion).

---

## Capability 1: ElevenLabs Streaming TTS

### API surface

ElevenLabs exposes `POST /v1/text-to-speech/{voice_id}/stream` which returns a
chunked HTTP response (MP3 frames). The current provider waits for the full body
before returning.

### Architectural seam: `elevenlabs-streaming-tts`

The streaming path requires a new output variant and IPC delivery model:

```
ProviderOutput::AudioStream {
    stream: Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>,
    content_type: String,   // "audio/mpeg"
    model_gen: String,
}
```

Because `IpcResponse` is a newline-framed JSON envelope, `AudioStream` cannot be
serialized directly. Two options:

**Option A — Blob-streaming sidecar handoff (recommended for Slice 1)**
The model-router writes audio chunks to the blob store (:9001) as a stream, then
returns a `BlobStream` reference in `IpcResponse`. The consumer (philote, membrane)
polls or subscribes to the blob stream endpoint. Lower implementation complexity;
reuses existing blob infrastructure.

**Option B — IPC chunked frames**
Add a `StreamFrame` IPC message type alongside `IpcResponse`. The model-router
sends a sequence of `StreamFrame { stream_id, seq, data_b64, done }` messages on
the socket. The consumer reassembles. Higher complexity but lower latency since
no intermediate blob store hop.

**Recommendation**: Implement Option A in Slice 1. The latency improvement from
removing the full-buffer wait is already a win. Option B is Slice 2 if the
blob-store round-trip proves too slow for voice UX.

### Slices

| Slice | Description |
|---|---|
| S1 | Add `/stream` call to `ElevenLabsProvider`, accumulate to blob store, return `Audio` with a streaming-origin flag. Shippable improvement with no IPC change. |
| S2 | Add `ProviderOutput::AudioStream` variant + `BlobStream` envelope in `IpcResponse`. Wire philote and membrane to consume streaming audio. |
| S3 | Option B IPC chunked frames if blob-store round-trip latency is unacceptable. |

### Also in scope: ElevenLabs STT — seam `elevenlabs-stt-surface`

ElevenLabs `POST /v1/speech-to-text` offers cloud STT as a complement to the local
`WhisperBackend`. Add `AudioTranscribe` support to `ElevenLabsProvider`:

- Request: multipart form with audio file + optional language/model params
- Response: `{ text: String, language_code: String, ... }`
- Returns `ProviderOutput::Text { content: transcript, ... }`

This is a single-slice addition with no new infrastructure. Routing config can
prefer ElevenLabs STT for languages Whisper handles poorly (multilingual), while
keeping `OnnxProvider` as the local/offline fallback.

---

## Capability 2: ONNX Music Analysis

### Motivation

Piano and organ playing generates a dense harmonic signal. Useful downstream
capabilities:

- **Piano transcription to MIDI** — convert a performance recording into a MIDI
  file for editing, notation, or RL feedback.
- **Chord/harmony analysis** — real-time chord label stream for a practice session.
- **Onset / note detection** — event stream of note onsets for timing feedback.

### Candidate models

| Model | Source | Format | Notes |
|---|---|---|---|
| `bytedance/piano_transcription` | HuggingFace / original repo | PyTorch → ONNX export needed | Industry reference; 88-key piano transcription → MIDI |
| Spotify Basic Pitch | `spotify/basic-pitch` | ONNX available | Multi-instrument; lighter weight; already exports ONNX |
| Google Magenta Piano Transcription | research.google | ONNX export feasible | High accuracy; heavier |
| Chord detection (Chordino, NNLS-Chroma) | Vamp plugins | C++ → requires FFI | Deferred |

**Recommended starting point**: Spotify Basic Pitch — ONNX weights are available,
multi-instrument support means it works for organ as well as piano, and the model
is small enough to run on CPU.

### Architectural seam: `onnx-music-analysis-surface`

New additions to `onnx-runner`:

```
crates/onnx-runner/src/backends/music.rs
  MusicAnalysisConfig { repo_id, prefer_quantized }
  MusicAnalysisOutput { midi_bytes: Vec<u8>, model_gen: String }
  MusicAnalysisBackend { session, ... }
  MusicAnalysisBackend::load(handle, config) -> Result<Self>
  MusicAnalysisBackend::analyze(wav_bytes: &[u8]) -> Result<MusicAnalysisOutput>
```

Input: 16 kHz mono WAV (reuses `audio::decode_wav` from the Whisper path).
Output: Raw MIDI bytes (SMF format). The caller serialises to file or forwards
over IPC.

### New output type: seam `midi-output-artifact`

MIDI is structured data, not a blob URI or text string. Add:

```rust
pub struct MidiArtifact {
    pub midi_bytes: Vec<u8>,
    pub duration_secs: f32,
    pub model_gen: String,
}

// In ProviderOutput:
MusicAnalysis(MidiArtifact),
```

And a corresponding `TaskKind::MusicAnalyze` variant.

### New task kind routing

```toml
# mesh-config.json example
[[routes]]
kind = "MusicAnalyze"
provider = "model-onnx-01"
```

### Sidecar endpoint

Add `POST /api/music-analyze` to the ONNX HTTP sidecar (port 11435):
- Body: raw WAV bytes
- Response: `{ midi_b64: "<base64>", duration_secs: 1.23, model_gen: "..." }`

### Slices

| Slice | Description |
|---|---|
| M1 | Export / pull Basic Pitch ONNX weights; implement `MusicAnalysisBackend::analyze` in `onnx-runner`; unit tests with a short piano WAV fixture. |
| M2 | Add `MidiArtifact` + `ProviderOutput::MusicAnalysis` + `TaskKind::MusicAnalyze` to model-router. Wire `OnnxProvider::invoke` for `MusicAnalyze`. |
| M3 | Add `/api/music-analyze` to the ONNX sidecar. Add smoke script. |
| M4 | Evaluate bytedance/piano_transcription (requires ONNX export step); swap or offer as alternate backend. |
| M5 | Real-time windowed analysis — slide a 5-second window over a live audio stream, emitting MIDI events incrementally. Depends on streaming infrastructure from `elevenlabs-streaming-tts` Slice 2. |

---

## Shared Infrastructure Note

Both capabilities ultimately motivate the same IPC extension: a way to deliver
non-text, potentially incremental artifacts from model-router to consumers.
The `BlobStream` reference approach (Streaming TTS S1) and the `MidiArtifact`
blob upload approach (Music Analysis M2) are both stop-gaps that fit inside the
existing `IpcResponse` JSON envelope. The long-term clean path is a dedicated
binary frame channel alongside the JSON socket — deferred to a future IPC
evolution proposal.

---

## Disposition

`proposed` — not yet scheduled. Prerequisites:

- Streaming TTS S1 depends on: `ElevenLabsProvider` refactor (straightforward).
- Streaming TTS S2 depends on: IPC schema change (coordinate with aiua + philote).
- Music M1 depends on: Basic Pitch ONNX weights confirmed available on HuggingFace Hub.
- Music M2 depends on: M1 green + `TaskKind` / `ProviderOutput` extension.

Suggested order: ElevenLabs STT (single slice, standalone) → Streaming TTS S1 →
Music M1+M2+M3 → Streaming TTS S2 (if blob-store latency is a problem).

---

## Cross-References

- [LOCAL_ONNX_INFERENCE_PROPOSAL.md](LOCAL_ONNX_INFERENCE_PROPOSAL.md) — Slice 2
  (WhisperBackend) shipped; `audio::decode_wav` and `log_mel_spectrogram` are
  reused directly by Music M1.
- [VOICE_MACHINE_PROPOSAL.md](VOICE_MACHINE_PROPOSAL.md) — `voice-transcribe-reentry`
  and `dedicated-voice-machine-component` seams; streaming TTS feeds into the voice
  machine component design.
- [MODEL_CONTROLLER_PROPOSAL.md](MODEL_CONTROLLER_PROPOSAL.md) — `TaskKind` and
  `ProviderOutput` are defined here; any new variants must be coordinated.
