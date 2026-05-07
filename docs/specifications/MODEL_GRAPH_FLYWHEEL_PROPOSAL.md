---
title: Model Graph, Routing Oracle, and Flywheel
doc_type: proposal
domain: tooling-execution
status: proposed
last_updated: 2026-05-06
tags:
- models
- routing
- flywheel
- fine-tuning
- cross-hotel
- vision
- asr
- embeddings
- self-healing
- mesh
related_docs:
- MODEL_GRAPH_AND_CONTEXT_1_PROPOSAL.md
- PARAKEET_ASR_PROPOSAL.md
- VOICE_TRAINING_ADMIN_SPEC.md
- ARCHITECTURE.md
task_refs:
- docs/task.md
proposal_id: model-graph-flywheel
supersedes: model-graph-context-1
implements: []
implemented_by:
- crates/model-router/src/controller.rs (TaskKind::RouteClassify, TaskKind::ImageGround, TaskKind::ImageOcr)
- crates/model-router/src/providers/ollama.rs (OllamaProvider multi-task)
- crates/model-router/src/bin/model-controller-ollama.rs
active_seams:
- model-graph-decision-layer
- model-oracle-routing
- model-flywheel
- cross-hotel-model-routing
- image-pipeline
- vision-model-provisioning
- model-operational-signals
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Model Graph, Routing Oracle, and Flywheel

## Goal

Build a self-improving model selection and routing system for the Philotic Stack. The system should:

1. Maintain a live graph of all model capabilities, trust tiers, and operational health — local and remote
2. Route any incoming task (text, audio, image) to the best available model using a fast local oracle
3. Fall back gracefully when specialized models are unavailable or degraded
4. Learn from operational signals over time through a fine-tuning flywheel
5. Work across the mesh — a hotel with a degraded local model should be able to delegate to a peer

This supersedes `model-graph-context-1`. That proposal was sound in structure but underspecified trust as a first-class dimension and did not cover cross-hotel routing, the fine-tuning loop, image pipelines, or HD-aware provisioning.

---

## What Is Already Shipped

As of 2026-05-06 on `develop`:

| Component | Status |
|---|---|
| `TaskKind::RouteClassify` | ✅ Shipped — model-router/src/controller.rs |
| `OllamaProvider` (TextGenerate + Embed + RouteClassify) | ✅ Shipped |
| `model-controller-ollama` binary | ✅ Shipped — Ollama health wait loop |
| `ProviderConfigs` ollama_embed_model + ollama_oracle_model | ✅ Shipped |
| `asr.setup` / `asr.status` IPC handlers | ✅ Shipped |
| Admin profile skills: inference.scripting, asr.admin, vision.admin | ✅ Shipped |
| `parakeet-runner` crate + `model-controller-parakeet` | ✅ Shipped |
| `model-controller-onnx` (Whisper, Kokoro, ONNX embed) | ✅ Shipped |
| FunctionGemma, EmbeddingGemma, Gemma 4 E4B local via Ollama | ✅ Available |
| `RouterTrainingRecord` + `token_count` + always-on `router_traces.db` | ✅ Shipped (Slice 1) |
| `extract_output_model_gen()` — populates `model_id` from `ProviderOutput` | ✅ Shipped (Slice 1) |

---

## Model Graph Schema

### Core Node Types

**`model_profile`** — Stable identity and static capability facts.

| Field | Type | Notes |
|---|---|---|
| `model_ref` | string | Canonical identifier, e.g. `nvidia/parakeet-tdt-0.6b-v3` |
| `provider` | enum | `ollama`, `mlx`, `subprocess`, `elevenlabs`, `gemini`, `openai` |
| `family` | string | `gemma`, `parakeet`, `falcon`, `florence`, `whisper`, ... |
| `version` | string | |
| `modalities` | string[] | `text`, `audio`, `image`, `video` |
| `max_context_tokens` | u32 | |
| `input_cost_per_million` | f64 | 0.0 for local |
| `output_cost_per_million` | f64 | |
| `latency_hint_ms` | u32 | Observed p50 |
| `disk_mb` | u32 | HD budget signal |
| `hosted_node_id` | Option<string> | null = not provisioned |
| `trust_tier` | enum | See trust model below |
| `source` | string | `benchmark`, `operator`, `observed` |

**`model_capability_score`** — Rated capability per dimension.

| Field | Type | Notes |
|---|---|---|
| `model_ref` | string | FK to model_profile |
| `dimension` | enum | See capability dimensions below |
| `score` | f32 | 0.0–1.0 |
| `source` | string | `benchmark`, `operator`, `observed` |
| `observed_at` | timestamp | |

Capability dimensions:
- `text_generation`, `code_generation`, `reasoning`, `instruction_following`
- `factual_accuracy`, `context_utilization`
- `tool_call_accuracy`, `tool_call_safety`, `structured_output`
- `audio_transcription`, `image_understanding`, `image_grounding`, `image_ocr`
- `embedding_quality`, `routing_classification`

**`model_operational_signal`** — Live health per (model, task_kind, node_id).

| Field | Type | Notes |
|---|---|---|
| `model_ref` | string | |
| `task_kind` | string | `text.generate`, `audio.transcribe`, `route.classify`, etc. |
| `node_id` | string | Which hotel is reporting |
| `status` | enum | `healthy`, `degraded`, `unavailable` |
| `error_rate_1h` | f32 | 0.0–1.0 rolling window |
| `latency_p50_ms` | u32 | |
| `latency_p95_ms` | u32 | |
| `last_healthy_at` | timestamp | |
| `degraded_since` | Option<timestamp> | |

**`model_fallback_chain`** — Ordered fallback list per (task_kind, node_id).

| Field | Type | Notes |
|---|---|---|
| `task_kind` | string | |
| `node_id` | string | `*` = applies to all nodes |
| `chain` | string[] | Ordered model refs, first healthy wins |

**`model_training_signal`** — Labeled examples for the fine-tuning flywheel.

| Field | Type | Notes |
|---|---|---|
| `signal_kind` | enum | `routing_decision`, `tool_call_outcome`, `transcription_correction`, `routing_correction` |
| `input_envelope` | json | The task envelope that arrived |
| `model_selected` | string | What was chosen |
| `outcome` | enum | `success`, `fallback_triggered`, `operator_correction` |
| `correction` | Option<json> | What the operator said should have happened |
| `session_id` | string | |
| `created_at` | timestamp | |

### Graph Edges

- `supports_task_kind` — model_profile → task_kind (weighted by capability score)
- `falls_back_to` — model_profile → model_profile (per task kind)
- `hosted_at` — model_profile → node (which hotel runs it)
- `observable_from` — model_operational_signal → node (which hotel is watching)
- `trained_on` — model_profile → model_training_signal (provenance chain)

---

## Trust Model

Trust is a first-class tier, not a tag. It directly gates the approval layer in philote.

| Tier | Meaning | Tool call behavior |
|---|---|---|
| `autonomous` | High accuracy, strong track record. | Preapproved for permitted tool classes without operator prompt. |
| `supervised` | Good general quality but risk classes require review. | Risky tool classes (shell, config) always surface for operator confirmation. |
| `restricted` | Unknown or unverified model. Default for new provisioning. | All tool calls require explicit approval regardless of class. |
| `quarantined` | Active degradation signal or policy violation. | Cannot initiate tool calls; read-only at most. |

Trust tier is set by the operator at provisioning time and can be upgraded by `muninn_decide` + operator confirmation after a track record is established. It degrades automatically when `error_rate_1h` crosses thresholds.

**Initial trust assignments:**

| Model | Tier |
|---|---|
| gemma4:e4b | supervised |
| functiongemma | supervised |
| embeddinggemma | autonomous |
| parakeet-tdt-0.6b-v3 | supervised |
| elevenlabs (TTS + STT) | supervised |
| gemini (cloud) | supervised |
| falcon-perception | restricted (until track record) |
| falcon-ocr / florence-2 | restricted (until track record) |

---

## Routing Oracle

The oracle replaces ad-hoc `supports()` matching with a graph-backed selection that returns a ranked candidate list with reasons.

### Oracle Query Input

```json
{
  "task_kind": "audio.transcribe",
  "required_modalities": ["audio"],
  "requesting_node_id": "bjork",
  "latency_slo_ms": 2000,
  "budget_hint": "local_preferred",
  "provider_hint": null,
  "trust_floor": "supervised"
}
```

### Oracle Query Output

```json
{
  "selected_model": "nvidia/parakeet-tdt-0.6b-v3",
  "fallback_chain": ["whisper-small", "gemini"],
  "score": 0.91,
  "reasons": ["best task fit", "healthy", "local", "latency within SLO"],
  "hard_filters_applied": ["trust_floor:supervised"],
  "confidence": "high"
}
```

### Oracle Implementation

`TaskKind::RouteClassify` is now wired. `OllamaProvider` routes it to `functiongemma` (270M, fast, purpose-built). Fine-tuning target: domain-adapted on philotic routing decisions via mlx-lm → HuggingFace → Ollama GGUF.

For the first slice, the oracle prompt is a structured system prompt encoding the model graph snapshot. Later slices replace this with direct graph query embedding.

---

## Fallback Chains

Each task kind has an explicit ordered fallback chain. The first entry with `status: healthy` wins.

| Task Kind | Chain | Notes |
|---|---|---|
| `text.generate` | gemma4:e4b → gemini | E4B is primary local model |
| `text.embed` | embeddinggemma → nomic-embed-text | Both local Ollama |
| `route.classify` | functiongemma → gemma4:e4b → gemini | E4B can classify if oracle down |
| `audio.transcribe` | parakeet → elevenlabs-stt → gemini | ElevenLabs STT as cloud fallback |
| `voice.synthesize` | kokoro → elevenlabs | Kokoro local ONNX first |
| `image.ocr` | falcon-ocr → florence-2 → gemma4:e4b → gemini | E4B multimodal as emergency fallback |
| `image.ground` | falcon-perception → gemma4:e4b → gemini | E4B can ground with prompting |
| `image.analyze` | gemma4:e4b → gemini | General vision description |

Gemma 4 E4B is the universal local fallback for all modalities it supports. It is multimodal (text + image + audio), already provisioned, and always-on.

---

## Cross-Hotel Routing

The mesh already carries `CapabilityAdvertisement` with `capability`, `node_id`, `latency_hint_ms`, and `concurrency_hint`. Cross-hotel routing extends the oracle query to include remote capability advertisements.

### How It Works

1. `model_operational_signal` includes `node_id` — each hotel reports its own model health
2. The oracle query includes `requesting_node_id` — the oracle knows which hotel is asking
3. Remote nodes' capability advertisements are projected into the fallback chain after local options
4. The `hosted_at` edge tracks which hotel runs each model

### Remote Fallback Example

```
bjork (requesting hotel): parakeet degraded, ElevenLabs STT unavailable
mesh advertisement: mbp-jane has parakeet healthy, latency 80ms
oracle selects: delegate audio.transcribe to mbp-jane/parakeet
```

The delegation path reuses the existing mesh routing and `EmitTask` infrastructure. The model-router on the requesting hotel emits the task envelope to the remote hotel's model controller.

### Trust in Cross-Hotel Context

Remote model nodes inherit the trust tier of the model profile, but add a `remote_trust_decay` factor — a remote model trusted as `supervised` is treated as `restricted` from the requesting hotel's perspective until a local operator explicitly grants cross-hotel promotion.

---

## ElevenLabs STT

ElevenLabs has a Speech-to-Text API alongside their TTS. Adding `AudioTranscribe` support to `ElevenLabsProvider` gives a cloud-quality ASR fallback when Parakeet is down.

**Implementation:**
- `ElevenLabsProvider::supports()` extends to include `TaskKind::AudioTranscribe`
- POST audio to `https://api.elevenlabs.io/v1/speech-to-text`
- Returns transcript text
- Same provider binary (`model-controller-elevenlabs`), no new binary needed
- Added to `audio.transcribe` fallback chain as position 2 (after Parakeet, before Gemini)

---

## Vision Models

### Task Kinds

Two new `TaskKind` variants:
- `ImageOcr` — extract text from image, output plain text
- `ImageGround` — locate objects/regions in image given text query, output structured JSON (bounding boxes + confidence)

### Model Options

**Falcon OCR** (`tiiuae/Falcon-OCR`):
- Text extraction specialist
- Python subprocess + inference script (trust_remote_code)
- HD cost: ~600MB–1.2GB depending on quantization
- Output: plain text

**Falcon Perception** (`tiiuae/Falcon-Perception`, ~600M):
- Grounding and segmentation specialist
- Python subprocess + inference script (trust_remote_code, MLX backend available)
- Output: JSON `{ "boxes": [...], "labels": [...], "scores": [...] }`

**Florence-2** (`microsoft/florence-2-base`, ~700MB):
- Handles both OCR and grounding in one model
- ONNX-exportable (lower HD pressure, no Python needed for inference)
- Weaker than Falcon specialists but single-model simplicity

**Recommended approach:** Florence-2 first (one model, ONNX-friendly, lower HD cost). Falcon Perception and Falcon OCR as optional upgrades registered via `vision.setup` when HD allows. The model graph tracks which is provisioned; the oracle picks accordingly.

### Inference Script Pattern

Admin philotes (with `inference.scripting` skill) write and iterate Python scripts:
- Script location: `~/.philotic/{profile}/scripts/{model_slug}_infer.py`
- CLI contract: `python script.py <image_path> [--query "..."] [--task ocr|ground]`
- Stdout contract: JSON with at minimum `{ "text": "..." }` for OCR or `{ "boxes": [...] }` for grounding
- Rust runner reads script path from hotel component config, falls back to embedded default

### `vision.setup` / `vision.status`

Mirror of `asr.setup`/`asr.status`:
- `vision.setup`: checks dependencies → writes component config → upserts guest record
- `vision.status`: reads provisioned model from config → checks guest health → returns status
- Both wired into `is_local_agent_tool` in `ipc.rs`
- Both added to admin profile `allowed_tools` and `asr` class extended to `vision` class

---

## Image Pipeline (Telegram)

Incoming Telegram photos flow:

```
Telegram photo message (file_id + optional caption)
  → membrane downloads via Bot API → saves to temp file
  → membrane POSTs to blob store (:9001) → gets blob_id
  → membrane sends IpcRequest::MediaAnalyze { blob_id, caption, mime }
  → philote: check caption for routing signal (Layer 1 reflex)
      → clear signal ("read this", "what does this say") → ImageOcr
      → grounding signal ("find the X", "where is the Y") → ImageGround
      → ambiguous / no caption → RouteClassify with image + caption
          → oracle queries model graph → returns intent
          → route to correct task kind
  → model-router dispatches to provisioned vision model
  → response back to Telegram
```

The RouteClassify intent classification for images uses FunctionGemma with an image description prefix when caption is absent (Gemma 4 E4B used to generate a one-line image description as oracle input).

---

## HD-Aware Provisioning

HD on mbp-jane is constrained. Provisioning is on-demand, not pre-loaded.

| Model | Disk | Strategy |
|---|---|---|
| gemma4:e4b | ~3.5GB | Always provisioned (primary model) |
| functiongemma | ~300MB | Always provisioned (oracle) |
| embeddinggemma | ~300MB | Always provisioned (embeddings) |
| nomic-embed-text | ~274MB | Keep as ONNX fallback |
| parakeet-tdt-0.6b-v3 | ~2GB Python env + ~1GB model | Provisioned via asr.setup |
| whisper-small | ~500MB | ONNX, keep as ASR fallback |
| florence-2 | ~700MB | Provisioned via vision.setup |
| falcon-perception | ~1.2GB | Optional upgrade via vision.setup |
| falcon-ocr | ~1GB | Optional upgrade via vision.setup |
| kokoro-82M | ~100MB | Always provisioned (TTS) |

Total always-on: ~4.5GB. On-demand models provisioned by admin philote as needed.

Ollama manages model memory via unified memory — models stay resident after first load but don't require separate disk caching. The HD cost is one-time model download.

---

## Self-Healing Watchdog

The self-healing loop runs in the hotel daemon:

1. **Signal collection**: Every `ProviderOutput` updates `model_operational_signal` for that model + task kind + node
2. **Threshold check**: If `error_rate_1h > 0.3` for a healthy model, mark as `degraded`; if `> 0.7`, mark `unavailable`
3. **Trust degradation**: `degraded` models have trust tier temporarily reduced by one level
4. **Oracle update**: Model graph health snapshot refreshes oracle context on each degradation event
5. **Admin notification**: Degraded specialist triggers a `hotel.notify` event to admin philote (Aria on mbp-jane)
6. **Auto-recovery attempt**: For subprocess models (Parakeet, vision), hotel attempts guest restart via existing 5s respawn loop
7. **Escalation**: If model remains unavailable after 3 respawn cycles, admin philote is notified to run `asr.setup` / `vision.setup`

---

## Fine-Tuning Flywheel

```
operational signals (routing decisions, tool call outcomes, transcription corrections)
  ↓ collected via model_training_signal records in context graph
  ↓ admin philote reviews + corrects via training.correct / routing.correct
  ↓ training.export → JSONL in HuggingFace SFT format
  ↓ mlx-lm fine-tune locally (LoRA, fits in 16GB unified memory)
     OR push to HuggingFace AutoTrain for cloud GPU
  ↓ fine-tuned weights → GGUF → Ollama (functiongemma-philotic:latest)
     OR fine-tuned weights → ONNX → embeddinggemma-philotic
  ↓ model graph records new model_profile node with trained_on edges
  ↓ operator promotes fine-tuned model in fallback chain
  ↓ loop continues
```

**Fine-tuning targets (Phase 1):**

| Model | Training Data | Format | Goal |
|---|---|---|---|
| FunctionGemma 270M | routing decisions + corrections | SFT function-call pairs | Domain-adapted routing oracle |
| EmbeddingGemma 300M | philotic conversation corpus + tool schemas | contrastive pairs | Domain-adapted semantic embeddings |

**Training framework:**
- Local: `mlx-lm` (LoRA, Apple Silicon, no GGUF conversion needed for mlx-lm server)
- Cloud: Unsloth on HuggingFace / Colab (for larger runs)
- Export: GGUF for Ollama deployment; MLX weights for mlx-lm server deployment

**mlx-lm server for fine-tuned models:**
Fine-tuned models can be served directly via `mlx_lm.server --model <weights_dir> --port 8080` without GGUF conversion. The Ollama provider's `base_url` config supports pointing to an mlx-lm server endpoint — same HTTP interface, different port.

---

## Implementation Slices

### Slice 1 — Operational Signal Training Tap ✅ COMPLETE
- `RouterTrainingRecord` extended with `token_count: Option<u64>`
- `router_traces.db` always-on at `~/.philotic/{PHILOTIC_PROFILE}/router_traces.db` (no env var required)
- `extract_output_model_gen()` populates `model_id` from `ProviderOutput.model_gen` with `task.model` fallback
- Idempotent schema migration for existing databases
- **Scope**: training tap only (controller-local, no agent cognitive visibility)
- Seam: `model-operational-signals`

### Slice 1b — Table-Datasource Guest (cognitive enrichment path)

The controller-side `router_traces.db` is the training tap. The *cognitive enrichment* path — where philote can see its own operational history at inference time — requires a separate data pipeline through the existing `datasource` crate runtime.

#### table-datasource binary (new: `crates/table-datasource`)
Parallel to `graph-datasource`. Uses `datasource::runtime::run_datasource_controller` with a new `SqliteTableProvider`:
- `table.query(table_name, sql, limit)` → `ProviderOutput::ResultSet`
- `table.insert(table_name, row_json)` → `ProviderOutput::Acknowledge`
- `table.configure(table_name, schema_sql)` → creates/migrates table
- `table.rolloff(table_name, max_rows, max_age_secs)` → deletes stale rows
- `table.stats(table_name)` → row count, last insert timestamp
- DB path from `PHILOTIC_TABLE_DB` env or hotel node config key `table_datasource.db_path`
- Multiple instances via different `guest_id` + `PHILOTIC_TABLE_GUEST_ID` env

#### Config-driven router-listener (refactor `crates/router-listener`)
Current: hard-coded to `whisper_training.db` + transcription events only.
Refactored: reads listener config from hotel node config at startup (`IpcRequest::GetConfig(key="router_listener.config")`). Config shape:
```json
{
  "listen_role": "router-listener",
  "filter_keys": {"philote_name": "aria"},
  "event_kinds": ["transcription_capture", "routing_signal"],
  "target_table_guest_role": "table-datasource",
  "table_name": "router_signals",
  "schema_map": {"session_id": "session_id", "turn_id": "turn_id", "provider_id": "provider_id", "model_id": "model_id", "latency_ms": "latency_ms", "outcome": "outcome", "ts": "timestamp"},
  "roll_off": {"max_rows": 50000, "max_age_secs": 604800},
  "adapter_script": null
}
```
Falls back to current whisper-only behavior when no config is present (backward compatible).
Python adapter: when `adapter_script` is set, spawn subprocess, pipe raw event JSON to stdin, read transformed JSON from stdout before inserting — allows arbitrary transformation without recompilation.

### Slice 1c — Philote `table.*` Tools + Cognitive Envelope Injection

#### Philote tools
- `table.setup(name, schema_sql, db_path?)` — writes `table_config:{agent_id}:{name}` node to agent graph, materializes table-datasource guest if not running
- `table.add_listener(name, listen_role, filter_keys, event_kinds, adapter_script?)` — writes `listener_config:{agent_id}:{name}` node, materializes router-listener guest
- `table.set_rolloff(name, max_rows, max_age_secs)` — updates rolloff config node in agent graph
- `table.set_rollup(name, interval_secs, query, output_table)` — schedules periodic aggregation job
- `table.query(name, sql, limit?)` — issues `table.query` task to the agent's table-datasource guest, returns rows
- `table.stats(name)` — row count, last write timestamp, rolloff eligibility

#### Agent-graph neighborhood
Config nodes scoped per agent:
- `table_config:{agent_id}:{table_name}` — schema, db_path, rolloff policy, rollup jobs
- `listener_config:{agent_id}:{listener_name}` — listen_role, filter_keys, schema_map, adapter path
- `Configures` edge from philote node → table_config and listener_config
- `Uses` edge from listener_config → table_config

Cognitive envelope injection: at session load, for each registered `table_config` node with a `context_query` property, execute that query against the table-datasource and inject result as `[Table: {name}]` section in the context envelope. Default `context_query` for `router_signals`: `SELECT provider_id, model_id, outcome, latency_ms, ts FROM router_signals ORDER BY ts DESC LIMIT 20`.

### Slice 2 — ElevenLabs STT
- Extend `ElevenLabsProvider` to support `TaskKind::AudioTranscribe`
- Add to fallback chain for `audio.transcribe`
- Wire into `model_profile` catalog seed

### Slice 3 — Model Graph Seed
- Define `model_profile` + `model_capability_score` storage in ansible-mesh-core
- Seed all currently wired models with initial scores and trust tiers
- Expose via read-only `model.list` admin tool

### Slice 4 — Vision Pipeline Foundation
- Add `TaskKind::ImageGround` + `TaskKind::ImageOcr` to controller
- `vision.setup` / `vision.status` IPC handlers in aiua
- Florence-2 inference script (default embedded, agent-overridable)
- `image.ocr` + `image.ground` tools wired in philote + admin catalog
- Seam: `vision-model-provisioning`

### Slice 5 — Image Pipeline (Telegram)
- Membrane: download Telegram photo → blob store → `MediaAnalyze` IPC
- Philote: Layer 1 caption reflex + RouteClassify fallback
- Route to `ImageOcr` / `ImageGround` / `MediaAnalyze` based on intent
- Seam: `image-pipeline`

### Slice 6 — Oracle Fine-Tuning Prep
- `routing.correct` tool (operator labels routing decisions as correct/wrong)
- `training.export` extended for routing signal format (HuggingFace SFT JSONL)
- mlx-lm fine-tune recipe in justfile

### Slice 7 — Cross-Hotel Model Routing
- Project remote capability advertisements into oracle fallback chains
- Mesh-aware oracle query (local first, remote as fallback)
- Remote trust decay policy
- Seam: `cross-hotel-model-routing`

### Slice 8 — Self-Healing Watchdog
- Threshold-based degradation detection in hotel daemon
- Admin notification on degradation events
- Auto-retry loop for subprocess model guests

---

## Open Questions

- **Florence-2 vs Falcon**: Start with Florence-2 (one model, ONNX path, lower HD cost)? Or go straight to Falcon specialists? Operator decision.
- **Trust promotion**: What observable track record is sufficient to promote a model from `restricted` to `supervised`? N successful tasks? Operator explicit confirmation?
- **Cross-hotel trust**: Should remote models ever reach `supervised` from the requesting hotel's perspective, or is `restricted` the permanent floor for remote delegation?
- **mlx-lm server vs Ollama**: After fine-tuning, deploy via mlx-lm server (no GGUF conversion, faster iteration) or always convert to GGUF for Ollama (uniform interface)? Both paths supported.
- **EmbeddingGemma ONNX**: Is there benefit to running EmbeddingGemma via ONNX runner (lower latency for high-frequency embedding calls) vs Ollama? Given ONNX reliability issues, Ollama is the default until ONNX proves stable.
