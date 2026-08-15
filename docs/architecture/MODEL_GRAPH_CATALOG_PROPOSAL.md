---
title: Model Graph Catalog Proposal
doc_type: proposal
domain: tooling-execution
status: implemented
last_updated: 2026-08-15
tags:
- model-controller
- model-catalog
- model-trust
- routing
- consolidation
- stale-branch-refresh
- huggingface
related_docs:
- MODEL_CONTROLLER_PROPOSAL.md
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
- SEAM_REGISTRY.md
- OUTBOUND_INTEGRATIONS.md
task_refs:
- docs/task.md
proposal_id: model-graph-catalog
implements:
- model-controller
implemented_by:
- crates/ansible-mesh-core/src/model_manager.rs
- crates/ansible-mesh-core/src/model_catalog_discovery.rs
- crates/ansible-mesh-core/src/heartbeat.rs
- crates/ansible-mesh-core/src/beacon.rs
- crates/aiua/src/main.rs
- crates/aiua/src/service/model_catalog_sync.rs
- crates/philotic-web/src/serve.rs
active_seams:
- model-graph-catalog-refresh
- catalog-vs-routing-authority
- model-trust-guidance
- model-graph-controller
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

The catalog owns:

- capability taxonomy such as `text.generate`, `voice.synthesize`,
  `speech.transcribe`, `text.embed`, and future `response.generate`
- provider, endpoint-family, model-family, and variant metadata
- modality, context-window, lifecycle, and coarse scoring hints
- source provenance and provider availability records that can be refreshed by
  a future model-graph controller
- seeded trust guidance for data-sensitivity classes such as `public`,
  `personal`, `lifegraph`, and `secret`
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

Implemented for the catalog/trust foundation slice.

The earlier proposal content on `origin/codex/model-graph-catalog` is directionally
useful, but the branch is not merge-ready. It is based on older runtime code and
currently carries a 56-file diff against `origin/develop`, including broad edits
across `aiua`, `philote`, `model-router`, `philotic-web`, sandbox code, and docs.

Do not merge `origin/codex/model-graph-catalog` wholesale.

Instead, extract the intended model-catalog work into small current seams and
delete the stale branch after those seams are landed or intentionally abandoned.

External ingestion is intentionally split into the `model-graph-controller`
seam. OpenRouter discovery and the first bounded Hugging Face metadata slice are
now implemented; benchmark feeds, provider-native inventories, and broader trust
refresh remain follow-ons rather than an excuse to make one universal crawler.

As of 2026-07-01, the live runtime already has a partial model graph in the form
of `ModelProfileRecord` plus health-aware routing:

- `ModelProfileRecord` stores per-node provider/task operational facts such as
  `model_ref`, `provider`, `task_kinds`, `trust_tier`, latency, error rate, and
  health status.
- `GraphDomain::observe_model_outcome` updates latency/error health after model
  dispatch.
- `GraphDomain::best_model_for` ranks node-local profiles for a task kind.
- `model-router` can substitute a healthier provider when the selected provider
  is degraded and no explicit provider hint was supplied.

That live profile layer is operational truth, not the static catalog itself. The
catalog refresh should join with it, not replace it.

## Current Slice

Make the old model graph catalog work current enough to merge safely, while
anchoring it to the live `ModelProfileRecord` and provider-key metadata already
present in the repo.

Implemented as of 2026-07-01:

- `ansible_mesh_core::model_manager` owns the first static
  `ModelCatalogRecord` seed and read-only `ModelCatalogProjection`.
- The seed reuses `ProviderKeySpec` display metadata and covers Gemini, OpenAI,
  OpenRouter, ElevenLabs, Ollama, ONNX, and MLX provider families.
- `philotic-web` exposes authenticated `GET /api/model-catalog`, which joins
  static catalog facts with live `ModelProfileRecord` entries and reports
  `routing_effect: none-read-only-projection`.
- Hotel-state sync now carries the sender hotel's own `ModelProfileRecord`
  entries, and receiving hotels upsert those remote profiles into their local
  graph so the projection can show mesh-wide live model facts.
- `ModelCatalogProjection` now includes seeded trust guidance. Trust gates are
  explainable records: public data may use proxy providers, personal data blocks
  proxy providers, and LifeGraph/secret data requires local providers by
  default.
- This slice does not change `model-router`, `model.manager.list`, provider
  selection, or fallback behavior.

Implemented and isolated-smoke-green as of 2026-08-15:

- `model.catalog.huggingface.ingest` is a code-owned SkillDAG capability record
  seeded only when absent. Its lifecycle is administrable through the landed
  skill plane; suspension or deprecation disables the periodic Hugging Face
  fetch without boot seeding silently restoring it.
- The `model-catalog-huggingface` binding is credential-free, `GET`-only, exact
  to `/api/models`, zero-redirect, 30-second/4-MiB bounded, and executes through
  the existing `egress-http-runner` with durable content-free audit. Its fixed
  query asks for at most 100 public models sorted by downloads, and the parser
  independently caps accepted rows at 100.
- The full `model_catalog_discovery.huggingface` snapshot persists source URL,
  fetch time, and repository revision. The separate
  `model_catalog.huggingface` read projection retains model id, provider-native
  task, conservatively mapped Philotic task kinds, license, library,
  downloads/likes, and provenance.
- Hugging Face repository facts do not enter `model_catalog.openrouter`, live
  `ModelProfileRecord` availability, or routing. Popularity and a model-card
  task are metadata claims, not execution proof.
- An isolated binary smoke proves the active SkillDAG gate, both exact governed
  bindings, runner execution, separate compact projections, Hugging Face
  task/license/revision provenance, durable audits, and closed session turns.
  Installed-runtime fetch, persistence, SkillDAG suspension, and selected-hotel
  execution are not yet watched live.

Routing conclusion as of 2026-07-01:

- The shared model graph is an input to routing policy, not the router itself.
- `model-router` should continue to honor explicit provider hints and make only
  local provider-family fallback decisions until hotel-owned capability routing
  provides remote dispatch and return-route guarantees.
- Cross-hotel model selection should happen in the hotel capability-routing
  layer: rank healthy `ModelProfileRecord` entries by task kind, prefer local
  providers unless policy says otherwise, verify peer reachability, then dispatch
  to the selected hotel over the normal mesh task path.

This slice should:

- audit `origin/codex/model-graph-catalog` and classify each changed file as:
  catalog schema, catalog projection, unrelated runtime drift, test-only update,
  or obsolete conflict
- recover only still-valid model catalog concepts onto current `develop`
- add the first provider-neutral catalog schema in the smallest shared crate that
  current code can consume without reviving old branch drift
- reuse existing shared metadata where it exists, especially
  `ansible_mesh_core::provider_keys::ProviderKeySpec`, instead of creating a
  third provider table
- seed a minimal catalog snapshot for currently supported provider families:
  Gemini, OpenAI, OpenRouter, Ollama-compatible, ElevenLabs, ONNX, and MLX
- keep endpoint families explicit, including planned/future endpoint families,
  without advertising unsupported runtime capability
- add focused tests for catalog shape and projection
- add one read-only projection that joins static catalog facts with live
  `ModelProfileRecord` health without changing routing
- share live model profile facts across hotels without replicating secrets,
  provider configs, or whole graph databases
- add trust/provenance records so catalog output can drive routing admission
  policy without becoming the router
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
- let static catalog scores override provider health, explicit provider hints,
  hotel reachability, or operator routing policy
- make `model-router` silently jump to remote hotels before hotel capability
  routing owns reachability checks and return-route semantics

## Refresh Plan

1. Create a clean branch from current `develop`.
2. Compare `origin/codex/model-graph-catalog` by intent, not by file diff.
3. Recreate the catalog schema as a small current patch, reusing
   `ProviderKeySpec` and existing `ModelProfileRecord` fields where possible.
4. Add seed data and tests for the currently materialized provider families.
5. Wire one read-only projection surface that reports catalog metadata plus live
   profile status.
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
- `catalog-live-profile-join`: joins static catalog records with
  `ModelProfileRecord` health and route traces for inspection only.
- `model-profile-hotel-state-sync`: shares each hotel's own live
  `ModelProfileRecord` entries through hotel-state sync.
- `hotel-capability-model-routing`: later routing slice where the hotel ranks
  local and remote profiles, verifies reachability, and dispatches over the mesh.
- `model-graph-controller`: partially implemented ingestion controller;
  OpenRouter and bounded Hugging Face metadata are present, while
  llm-stats-style feeds and additional provider-native catalogs remain deferred.

## Validation

Minimum validation for the refresh slice:

- targeted unit tests for catalog schema and seeded snapshot
- targeted unit tests for trust decisions and backward-compatible hotel-state
  sync
- targeted model-router tests if the catalog is consumed there
- `cargo check` for touched crates

Do not claim live-green from catalog metadata alone. Catalog correctness is
compile/test truth until a runtime surface consumes it in a watched route.

## Open Questions

- Resolved for this slice: catalog/trust records live first in
  `ansible-mesh-core` and project through `philotic-web`.
- Resolved for the first Hugging Face slice: six-hour cadence shared with the
  hotel catalog job, fixed top-100 request, parser-side top-100 cap, 4-MiB
  response limit, governed placement, and no credential.
- Deferred: stale-source semantics, pagination beyond the bounded top 100,
  benchmark feeds, and broader controller placement policy.
- Deferred: routing admission integration, especially how LifeGraph sensitivity
  is inferred on mixed-context turns.
