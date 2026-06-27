---
title: Model Graph Catalog Proposal
doc_type: proposal
domain: tooling-execution
status: accepted-current-slice
last_updated: 2026-06-22
tags:
- model-controller
- model-catalog
- routing
- consolidation
- stale-branch-refresh
related_docs:
- MODEL_CONTROLLER_PROPOSAL.md
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
- SEAM_REGISTRY.md
task_refs:
- docs/task.md
proposal_id: model-graph-catalog
implements:
- model-controller
implemented_by: []
active_seams:
- model-graph-catalog-refresh
- catalog-vs-routing-authority
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Model Graph Catalog Proposal

## Goal

Create one provider-neutral model catalog that Philotic can use to describe
model families, capabilities, endpoint families, variants, and coarse selection
hints without turning static metadata into a second runtime router.

This proposal also dispositions the stale remote branch
`origin/codex/model-graph-catalog`.

## Core Recommendation

Refresh the model catalog work as a current `develop` slice instead of merging
the stale branch wholesale.

The catalog should own:

- capability taxonomy such as `text.generate`, `voice.synthesize`,
  `speech.transcribe`, `text.embed`, and future `response.generate`
- provider, endpoint-family, model-family, and variant metadata
- modality, context-window, lifecycle, and coarse scoring hints
- stable references that `model-router`, `model.manager.list`, and future turn
  routing surfaces can project consistently

The catalog must not own:

- live node availability
- queue depth, active jobs, or hotel reachability
- auth tokens or token refresh
- final per-turn provider selection

Authority split:

- model catalog = static facts and ranking hints
- hotel/node registry = live availability and reachability
- model-router/controller = provider execution and provider-native rendering
- hotel policy = auth, placement, and runtime admission control
- turn routing plan = actual staged choice for a specific inbound turn

If those boundaries blur, the catalog becomes a shadow control plane. It would
be very elegant right up until it starts lying with confidence.

## Disposition

Accepted for refresh/re-slice.

The earlier proposal content on `origin/codex/model-graph-catalog` is directionally
useful, but the branch is not merge-ready. It is based on older runtime code and
currently carries a 56-file diff against `origin/develop`, including broad edits
across `aiua`, `philote`, `model-router`, `philotic-web`, sandbox code, and docs.

Do not merge `origin/codex/model-graph-catalog` wholesale.

Instead, extract the intended model-catalog work into small current seams and
delete the stale branch after those seams are landed or intentionally abandoned.

## Current Slice

Make the old model graph catalog work current enough to merge safely.

This slice should:

- audit `origin/codex/model-graph-catalog` and classify each changed file as:
  catalog schema, catalog projection, unrelated runtime drift, test-only update,
  or obsolete conflict
- recover only still-valid model catalog concepts onto current `develop`
- add the first provider-neutral catalog schema in the smallest shared crate
  that current code can consume without reviving old branch drift
- seed a minimal catalog snapshot for currently supported provider families:
  Gemini, OpenAI, Ollama-compatible, ElevenLabs, ONNX, and MLX
- keep endpoint families explicit, including planned/future endpoint families,
  without advertising unsupported runtime capability
- add focused tests for catalog shape and projection
- update `MODEL_CONTROLLER_PROPOSAL.md`, `ARCHITECTURE_STATUS.md`, and
  `docs/task.md` only with current, proven truth

This slice should not:

- replace live routing or `NodeRegistry`
- import stale `aiua`, `philote`, `philotic-web`, or sandbox edits from the old
  branch without a fresh justification
- claim provider support just because the catalog can represent it
- make model scores look more precise than they are
- make `model.manager.list` catalog-backed until the projection is actually
  implemented and tested

## Refresh Plan

1. Create a clean branch from current `develop`.
2. Compare `origin/codex/model-graph-catalog` by intent, not by file diff.
3. Recreate the catalog schema as a small current patch.
4. Add seed data and tests.
5. Wire one read-only projection surface.
6. Run targeted crate tests and a model-router check.
7. Delete `origin/codex/model-graph-catalog` after the current seams land.

## Candidate Seams

- `model-catalog-schema`: provider, endpoint-family, model-family, variant,
  capability, modality, lifecycle marker, and coarse score records.
- `model-catalog-seed`: minimal static seed for supported provider families.
- `model-catalog-projection`: read-only projection into `model.manager.list`
  or another bounded model catalog query surface.
- `turn-routing-catalog-input`: deferred integration where turn routing can use
  catalog facts as hints without surrendering routing authority.

## Validation

Minimum validation for the refresh slice:

- targeted unit tests for catalog schema and seeded snapshot
- targeted model-router tests if the catalog is consumed there
- `cargo check` for touched crates

Do not claim live-green from catalog metadata alone. Catalog correctness is
compile/test truth until a runtime surface consumes it in a watched route.

## Open Questions

- Should the catalog live first in `ansible-mesh-core` compatibility space, a
  primitive model crate, or `model-router` with reexports?
- What lifecycle vocabulary do we need now: `supported`, `configured`,
  `planned`, `experimental`, `deprecated`?
- Should local MLX fleet data be static catalog metadata, live registry data, or
  a joined projection of both?
- Which operator surface should expose read-only catalog inspection first?
