# Voice Training Admin Pipeline Spec

**Status**: Draft
**Scope**: Admin-level philote tooling for managing ASR voice training data
**Date**: 2026-04-29

---

## Overview

The `WhisperTrainingStorage` infrastructure already exists in `ansible-mesh-core` and the `router-listener` guest automatically captures voice turns. What's missing is operator-facing tooling that an admin philote can use to inspect, correct, manage, and export training data — and eventually trigger fine-tuning runs.

---

## New Tools Required

### `training.list`

List recent voice training samples with metadata.

```json
{
  "type": "object",
  "properties": {
    "limit": { "type": "integer", "description": "Number of samples to return (default 20, max 200)" },
    "filter": {
      "type": "string",
      "enum": ["all", "uncorrected", "eligible", "exported"],
      "description": "Filter samples by correction state"
    },
    "agent_id": { "type": "string", "description": "Filter to a specific agent" }
  }
}
```

Returns: `sample_id`, `turn_id`, `agent_id`, `raw_transcript`, `corrected_transcript` (if any), `confidence`, `model_gen`, `timestamp`, `training_eligible`.

### `training.correct`

Apply an operator correction to a captured sample. Marks it `training_eligible = true`.

```json
{
  "type": "object",
  "properties": {
    "turn_id": { "type": "string", "description": "The turn to correct" },
    "corrected_transcript": { "type": "string", "description": "The ground-truth transcript" }
  },
  "required": ["turn_id", "corrected_transcript"]
}
```

### `training.export`

Export eligible samples to a file the fine-tuning pipeline can consume.

```json
{
  "type": "object",
  "properties": {
    "format": {
      "type": "string",
      "enum": ["huggingface", "nemo"],
      "description": "Output format. nemo emits one-JSON-per-line manifest; huggingface emits dataset-compatible JSON."
    },
    "output_path": { "type": "string", "description": "Absolute path for the export file" },
    "limit": { "type": "integer", "description": "Max samples to export (default: all eligible)" }
  },
  "required": ["format", "output_path"]
}
```

After export, marks exported samples so they are not re-exported. Operator must explicitly re-set eligibility if re-export is needed.

### `training.status`

Returns a summary: total samples captured, uncorrected, eligible, exported. Breakdown by `model_gen` tag (distinguishes Whisper vs Parakeet samples).

---

## New IPC Requests

The above tools require new `IpcRequest` variants in `philotic-client/src/lib.rs` and handlers in `aiua/src/service/ipc.rs`:

```
IpcRequest::ListTrainingSamples { agent_id: Option<String>, limit: usize, filter: TrainingFilter }
IpcRequest::CorrectTrainingSample { turn_id: String, corrected_transcript: String }
IpcRequest::ExportTrainingSamples { format: TrainingExportFormat, output_path: String, limit: Option<usize> }
IpcRequest::GetTrainingStatus {}
```

The aiua IPC server already holds access to the SQLite graph DB; the `WhisperTrainingStorage` instance is owned by `router-listener`, not aiua. Two options:

**Option A** — aiua opens a read-write connection to the same training DB path alongside `router-listener`. Both use `Arc<Mutex<Connection>>`; SQLite WAL mode handles concurrent access safely.

**Option B** — Expose training ops via `router-listener` IPC inbox: admin philote sends tasks to `router-listener`'s role inbox, gets responses back via paracrine mechanism.

**Recommendation**: Option A. Simpler, no extra routing layer. The training DB path is already configurable via `PHILOTIC_TRAINING_DB` env var in `router-listener`. Add the same env var support to aiua startup.

---

## Toolset Profile Changes

Add to `admin` profile:
```
"training.list", "training.correct", "training.export", "training.status"
```

These tools are high-value but low-risk. No operator approval gate needed for `training.list` and `training.status`. `training.correct` and `training.export` should be pre-approved for admin roles.

Add a new `training-admin` skill to `seed_abstract_skill_catalog`:
```
"training.admin" — Run a training data review session: list uncorrected samples,
apply corrections, check eligibility count, and export when ready.
Implied tools: training.list, training.correct, training.export, training.status.
```

---

## Fine-Tuning Trigger

Out of scope for this tool layer, but the export output feeds:
- **Whisper fine-tuning**: standard HuggingFace `transformers` fine-tuning script
- **Parakeet fine-tuning**: NeMo LoRA fine-tune (`scripts/finetune-parakeet.py`, see `PARAKEET_ASR_PROPOSAL.md`)

The admin philote can use `bash.exec` (with approval) to trigger these scripts after export. A future `training.finetune` tool could wrap this once the scripts are stable.

---

## Implementation Slices

**Slice 1 — IPC + aiua handler (2 days)**
- `IpcRequest::ListTrainingSamples` / `GetTrainingStatus` read-only handlers in aiua
- aiua opens shared training DB connection (WAL mode, read-write)
- `training.list` and `training.status` tools registered in catalog + admin profile

**Slice 2 — Write ops (1 day)**
- `IpcRequest::CorrectTrainingSample` handler
- `training.correct` tool registered
- Philote handler (admin only via role check)

**Slice 3 — Export (1 day)**
- `IpcRequest::ExportTrainingSamples` handler
- `training.export` tool with `huggingface` and `nemo` format support
- `phil training export` CLI subcommand gains `--format nemo` flag

**Slice 4 — Skill + profile wiring (0.5 days)**
- `training.admin` skill in abstract skill catalog
- Admin profile update
- Bjork's admin role (if any) wired with training skill

---

## Open Questions

1. **Training DB path convention**: Currently `router-listener` reads `PHILOTIC_TRAINING_DB` env var defaulting to `whisper_training.db` in the current directory. Should this be profile-namespaced like the graph DB (`~/.philotic/<profile>/training.db`)? Yes — align with profile dir pattern.

2. **Multi-agent training**: The `agent_id` filter in `training.list` allows inspecting any agent's samples. Is this admin-only or should orchestrators be able to see their own? Recommend admin-only for now; grant self-access in a follow-up.

3. **Audio file management**: `audio_path` points to a file on disk. Export needs to bundle audio files or confirm they're accessible from the fine-tuning machine. For cross-machine export (Mac → VPS), audio must be transferred separately via blob store or scp.
