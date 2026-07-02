---
title: Transcription Flywheel — Router-Listener, Training Capture, and Whisper Fine-Tune Loop
doc_type: proposal
domain: voice-ai
status: proposed
disposition: accepted-current-slice
last_updated: 2026-04-22
tags:
- whisper
- transcription
- router-listener
- training
- flywheel
- huggingface
- onnx
- voice
- rl-flywheel
related_docs:
- LOCAL_ONNX_INFERENCE_PROPOSAL.md
- EMBEDDINGS_TRAINING_DATA_PROPOSAL.md
- VOICE_MACHINE_PROPOSAL.md
- MODEL_CONTROLLER_PROPOSAL.md
proposal_id: transcription-flywheel
implements:
- local-onnx-inference
implemented_by: []
active_seams:
- router-listener-primitive
- whisper-training-capture
- huggingface-training-loop
- onnx-model-hot-swap
---

# Transcription Flywheel — Router-Listener, Training Capture, and Whisper Fine-Tune Loop

## Goal

Close the Whisper accuracy loop: every voice interaction Bjork (or any philote) handles
becomes potential training data that flows through operator correction, export to
HuggingFace, fine-tuning, and ONNX hot-swap — automatically tightening transcription
quality over time without manual intervention.

This proposal introduces:

1. **Router-Listener** — a new IPC guest primitive that passively observes model-router
   envelopes and fires capture rules
2. **Whisper Training Store** — durable SQLite capture of `(audio, transcript)` pairs
   with an operator correction channel
3. **HuggingFace Training Loop** — export → push to HF Hub → trigger fine-tune →
   pull ONNX checkpoint → hot-swap in onnx-runner

---

## Context

Bjork's voice pipeline is:

```
Telegram voice message
  → blob store (ephemeral WAV)
  → model-router AudioTranscribe → WhisperBackend (onnx-runner)
  → transcript → philote cognitive loop
```

The `blob_download_url` and transcript both pass through `model-controller-onnx`
(at `crates/model-router/src/providers/onnx.rs`) at the moment of transcription.
Nothing captures them — they are lost as soon as the turn completes.

The router-listener adds a passive observer at that exact moment.

---

## Architecture

### 1. Router-Listener Primitive

A **router-listener** is an IPC guest that subscribes to a dedicated inbox role
(`role = "router-listener"`). The model-router fans out a capture envelope there
after each successful task dispatch — fire-and-forget, non-blocking.

```
model-controller-onnx
  transcribe(wav_bytes) → {text, model_gen}
  emit_text_response → philote        ← existing path
  emit_capture_envelope → router-listener   ← new fan-out (fire-and-forget)
```

#### Capture envelope schema (AudioTranscribe)

```json
{
  "kind": "transcription_capture",
  "session_id": "...",
  "turn_id": "...",
  "agent_id": "...",
  "transcript": "...",
  "model_gen": "whisper-small@abc123",
  "blob_download_url": "http://127.0.0.1:9001/download/sha256-...",
  "timestamp": 1745000000
}
```

The blob URL is available from the inbound task's attachment in `runtime.rs`
and is passed through the capture envelope. The router-listener **immediately**
downloads the WAV on receipt — the blob store is ephemeral and may be GC'd
before a delayed copy attempt.

#### Fan-out point

In `crates/model-router/src/runtime.rs`, after a successful `AudioTranscribe`
provider result, spawn a fire-and-forget task:

```rust
if task_kind == "voice.transcribe" && outcome == "success" {
    if let Some(blob_url) = extract_blob_url_from_task(&task_value) {
        tokio::spawn(emit_capture_envelope(
            capture_ipc_client.clone(),
            &reply,
            transcript.clone(),
            model_gen.clone(),
            blob_url,
        ));
    }
}
```

This is gated by `PHILOTIC_ROUTER_CAPTURE_ENABLED=true` to avoid unintended
side effects on existing deployments.

#### Router-listener binary

New crate: `crates/router-listener/src/main.rs`

- Registers with IPC as `role = "router-listener"`
- Subscribes to inbox
- On `transcription_capture`:
  1. Download audio from `blob_download_url` → write to `$PHILOTIC_TRAINING_AUDIO_DIR/<turn_id>.wav`
  2. Insert `WhisperTrainingSample` row (see DB schema below)
  3. Log: `[capture] turn_id={} agent={} model_gen={} audio_path={}`
- On `transcription_correction`:
  1. Update `corrected_transcript`, set `training_eligible = 1`, set `correction_source`

---

### 2. Whisper Training Store

New table added alongside `router_traces` in `crates/ansible-mesh-core/src/router_trace.rs`
(or new module `whisper_training.rs`).

#### Schema

```sql
CREATE TABLE whisper_training_samples (
    sample_id              TEXT PRIMARY KEY,   -- ULID
    agent_id               TEXT NOT NULL,
    session_id             TEXT NOT NULL,
    turn_id                TEXT NOT NULL,
    raw_transcript         TEXT NOT NULL,      -- from Whisper, unmodified
    corrected_transcript   TEXT,               -- NULL until operator corrects
    correction_source      TEXT,               -- "operator" | "auto"
    model_gen              TEXT NOT NULL,      -- "whisper-small@abc123"
    audio_path             TEXT,               -- absolute path to copied WAV
    timestamp              INTEGER NOT NULL,
    training_eligible      INTEGER NOT NULL DEFAULT 0  -- 1 when ready for export
);

CREATE INDEX idx_whisper_training_ts
    ON whisper_training_samples (timestamp DESC);

CREATE INDEX idx_whisper_training_eligible
    ON whisper_training_samples (training_eligible, timestamp DESC);
```

#### Rust types

```rust
pub struct WhisperTrainingSample {
    pub sample_id: String,
    pub agent_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub raw_transcript: String,
    pub corrected_transcript: Option<String>,
    pub correction_source: Option<String>,
    pub model_gen: String,
    pub audio_path: Option<String>,
    pub timestamp: u64,
    pub training_eligible: bool,
}

pub trait WhisperTrainingStorage: Send + Sync {
    fn insert_sample(&self, sample: &WhisperTrainingSample) -> Result<()>;
    fn update_correction(&self, turn_id: &str, corrected: &str, source: &str) -> Result<()>;
    fn list_eligible(&self, limit: usize) -> Result<Vec<WhisperTrainingSample>>;
    fn mark_exported(&self, sample_ids: &[&str]) -> Result<()>;
}
```

---

### 3. Human Correction Side Channel

Operator sends `/correct <turn_id> <corrected text>` via Telegram.

**Flow:**

```
operator: /correct 01JT3K4M... "what time is the meeting tomorrow"
philote: parse SlashCommand::Correct { turn_id, text }
philote → EmitTask → router-listener ("transcription_correction")
router-listener: UPDATE whisper_training_samples SET corrected_transcript=..., training_eligible=1
                 WHERE turn_id=...
philote: reply "Correction recorded."
```

**Correction envelope:**

```json
{
  "kind": "transcription_correction",
  "turn_id": "01JT3K4M...",
  "corrected_transcript": "what time is the meeting tomorrow",
  "correction_source": "operator"
}
```

**philote changes:**
- New `SlashCommand::Correct { turn_id: String, text: String }` variant
- Parsed as `/correct <turn_id> <rest of line>`
- Routed to `handle_session_control_command` → emits correction envelope

---

### 4. HuggingFace Training Loop

The full fine-tune loop:

```
whisper_training_samples (training_eligible=1)
  → export as HF Dataset (CommonVoice format)
  → push to HF Hub (private dataset repo)
  → trigger fine-tune job (HF AutoTrain or local Trainer)
  → export fine-tuned checkpoint to ONNX
  → pull ONNX model → hot-swap in onnx-runner
```

#### Export format (CommonVoice / HF Whisper)

```json
{
  "audio": { "path": "/training/01JT3K4M.wav" },
  "sentence": "what time is the meeting tomorrow",
  "locale": "en",
  "model_gen": "whisper-small@abc123"
}
```

Exported as JSONL via `phil training export --format=hf-whisper`.

#### HuggingFace integration

New `phil training` CLI subcommand (in `crates/phil/src/`):

```
phil training list                    # show captured samples, correction status
phil training export [--eligible]     # export to JSONL
phil training push                    # push dataset to HF Hub
phil training fine-tune               # trigger HF AutoTrain job or local
phil training pull-model <run-id>     # download checkpoint
phil training onnx-export <ckpt>      # convert checkpoint → ONNX
phil training hot-swap <onnx-path>    # signal onnx-runner to reload
```

**HF Hub push** (`phil training push`):
- Uses `huggingface_hub` Python lib (CLI wrapper) or native HF API
- Target: `$HF_REPO_ID` (e.g. `likesjx/bjork-whisper-training-data`)
- Auth: `$HF_TOKEN` (from hotel key vault)

**Fine-tune trigger** (`phil training fine-tune`):
- Option A: HF AutoTrain — POST to `https://huggingface.co/api/autotrain` with dataset + model config
- Option B: Local `transformers` fine-tune script wrapping `openai/whisper-small`
  with the exported JSONL as train split
- Option C: HF Spaces — trigger a dedicated fine-tune Space

For Bjork's scale (hundreds of corrections), local fine-tune on MacBook Air M-series
is realistic: Whisper small is 244M params, LoRA or full fine-tune on `<1000` samples
runs in minutes.

**ONNX export and hot-swap:**
- `optimum-cli export onnx --model <checkpoint> whisper-finetuned.onnx`
- Hot-swap via existing `PHILOTIC_WHISPER_MODEL_RELOAD` signal path in `onnx-runner`
  (if not yet built, add `SIGHUP` or file-watch in `WhisperBackend`)

---

## Phases

### Phase 1 — Router-Listener + Capture (Slice 1–3)

**Slice 1**: `WhisperTrainingStorage` trait + `SqliteWhisperTrainingStorage` in `ansible-mesh-core`

**Slice 2**: Fan-out in `model-router/src/runtime.rs` after `AudioTranscribe` success;
fan-out in `onnx.rs` to pass `model_gen` + `blob_url` through `ProviderOutput`

**Slice 3**: New `crates/router-listener` binary — IPC guest, `transcription_capture` handler,
audio copy, DB insert

### Phase 2 — Operator Correction (Slice 4)

**Slice 4**: `SlashCommand::Correct` in `philote/src/commands.rs` + runtime handler +
`transcription_correction` routing to router-listener

### Phase 3 — HuggingFace Loop (Slice 5–7)

**Slice 5**: `phil training list/export` — read from SQLite, emit JSONL

**Slice 6**: `phil training push` — HF Hub dataset upload

**Slice 7**: `phil training fine-tune / pull-model / onnx-export / hot-swap`

---

## Open Questions

**Confidence score**: `WhisperBackend::transcribe` currently returns only `text` + `model_gen`.
Adding avg log-prob as a confidence score (`0.0–1.0`) would let the system filter out
low-confidence transcriptions before offering them for correction. This is a small
addition to `TranscribeOutput` in `crates/onnx-runner/src/backends/transcribe.rs`.

**Auto-eligible threshold**: If confidence > 0.95, mark `training_eligible = 1` without
requiring operator correction. This is opt-in (`PHILOTIC_TRAINING_AUTO_ELIGIBLE=true`).

**Audio format**: Blob store currently stores OGG (from Telegram). Whisper training
expects WAV (16kHz mono PCM). The router-listener should transcode on copy using `ffmpeg`
or `rodio`. Alternatively: store OGG and transcode at export time.

**Multi-hotel**: Router-listener is node-local by design (same as router traces). Each
hotel trains on its own agent's voice data. Cross-hotel dataset merging is a Phase 4 concern.

**HF AutoTrain vs local**: AutoTrain has a cost. Local fine-tune on M-series is free
but requires leaving the machine running. Decision deferred to Phase 3 based on
sample volume at that time.

---

## Relationship to Existing Work

- **`LOCAL_ONNX_INFERENCE_PROPOSAL`** (`proposed`): This proposal extends Phase 2
  (Whisper fine-tune) of that seam.
- **`EMBEDDINGS_TRAINING_DATA_PROPOSAL`** (`proposed`): Parallel flywheel for embeddings.
  Same HF Hub / training loop pattern; different model and feedback signal.
- **`VOICE_MACHINE_PROPOSAL`** (`accepted-current-slice`): Whisper transcription is already
  wired. This adds the training feedback path.
- **`ResourceType::RouterListener`** in `ansible-mesh-core/src/resources.rs`: Already named.
  This proposal gives it concrete implementation.

---

## Disposition

`proposed` — no implementation started.

Priority: Phase 1 (capture) is low-risk, high-signal. Bjork produces multiple voice
transcriptions per day. Starting capture now means the training store is already
populated when Phase 3 (HF loop) is ready.

Phase 3 (HuggingFace) is sequenced after Phase 2 (operator correction) because
uncorrected training data from an imperfect baseline model is training noise.
