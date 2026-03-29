---
title: "Model Graph Catalog Proposal"
doc_type: proposal
domain: tooling-execution
status: proposed
last_updated: 2026-03-26
tags:
  - model-controller
  - model-catalog
  - models
  - routing
  - endpoints
related_docs:
  - MODEL_CONTROLLER_PROPOSAL.md
  - TURN_ROUTING_PLAN_PROPOSAL.md
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
  - SEAM_REGISTRY.md
task_refs:
  - docs/task.md
proposal_id: model-graph-catalog
implements:
  - model-controller
implemented_by:
  - shared-model-catalog-schema
active_seams:
  - model-graph-catalog
  - catalog-vs-routing-authority
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
---

# Model Graph Catalog Proposal

## Goal

Define one canonical, provider-neutral model graph for the models Philotic knows how to
talk about, including:

- capability tree and functional families
- provider and endpoint-family metadata
- model and variant records
- coarse scoring axes like capability, speed, thinking depth, and cost efficiency
- explicit linkage to the model-controller boundary without turning static metadata into live routing truth

## Core Recommendation

Introduce a shared model catalog as static metadata, not as a second routing system.

The catalog should own:

- capability taxonomy such as `text.generate`, `voice.synthesize`, `speech.transcribe`, and future `response.generate`
- provider records and endpoint stems
- model families, variants, modalities, context windows, and coarse score vectors
- stable references that `model-controller` and `model.manager.list` can project consistently

The catalog should not own:

- live node availability
- queue depth, active jobs, or hotel/node reachability
- current auth tokens or token refresh
- per-turn provider selection by itself

That authority split matters:

- model graph = static facts and scoring hints
- node registry / advertisements = live mesh truth
- model-controller = provider execution and contract shaping
- hotel = auth and runtime policy

If those get blurred, we end up with an impressive graph that quietly becomes a shadow control plane. Distributed systems do enjoy inventing extra governments when unsupervised.

## Disposition

`proposed`

## Current Slice

Land the first schema and seed snapshot only.

This slice should:

- define a provider-neutral catalog shape in shared code
- seed the first catalog snapshot for currently supported providers:
  - Gemini
  - ElevenLabs
  - ONNX
  - MLX
- make endpoint families explicit, including reserved future families such as Gemini live/native audio
- keep unsupported future models out of the seeded supported-model set while still allowing endpoint-family reservation
- document how Gemini 3.1 Flash Live informs the schema without falsely claiming runtime support today

This slice should not yet:

- replace `NodeRegistry` routing
- claim that `model.manager.list@1` is already catalog-backed
- auto-discover provider fleets into the catalog
- make the catalog the owner of runtime health or node placement

## Gemini 3.1 Flash Live As Exemplar Input

The Gemini 3.1 Flash Live model card is useful design input because it exposes the exact
kinds of metadata Philotic should be able to represent:

- endpoint/distribution channels
- multimodal inputs and outputs
- context and output token budgets
- explicit live/audio interaction intent
- variant-level scoring distinctions such as higher-thinking vs minimal-thinking operation

For this proposal, treat that card as schema pressure, not implementation truth.

In other words:

- yes, the graph should be able to represent Gemini live/audio families and variant weights
- no, Philotic should not advertise Gemini 3.1 Flash Live as supported until the actual controller/runtime path exists

## Recommended Shared Shape

At minimum, the shared catalog should carry:

1. capability tree
2. provider records
3. endpoint-family records
4. model records
5. variant records
6. coarse scoring vector

Suggested coarse scoring vector:

- `capability`
- `speed`
- `thinking`
- `cost_efficiency`
- `tool_use`
- `audio_native`

These are intentionally approximate ranking aids, not benchmark absolutism pretending to be science.

## Authority Split With Existing Surfaces

### `model.manager.list@1`

Should eventually project:

- live nodes from the registry
- plus static catalog metadata for recognized `model_ref` values

It should not confuse:

- "this node currently serves model X"
- with "the catalog says model family X generally supports audio and has high thinking depth"

### `ProviderRegistry`

Should eventually use the catalog as the canonical source for provider/model capability metadata,
while still relying on provider/runtime truth for:

- whether a provider is actually configured
- whether a fleet is healthy
- whether a local or remote execution path is available

### `model-controller`

Should own rendering the shared contract into provider-native requests.

The graph should inform controller behavior; it should not replace provider-specific execution code.

### `TurnRoutingPlan`

The model graph should feed the turn routing planner, not bypass it.

In the target shape:

- model graph answers "what can these models and endpoint families generally do?"
- turn routing plan answers "which staged path should this actual inbound turn use?"

That is especially important for streaming voice turns, where ingress,
cognition, and egress may legitimately want different providers without creating
three competing owners for one conversation turn.

## Next Seams

- catalog projection into `model.manager.list@1`
- turn-routing-plan integration so stage selection can use catalog metadata explicitly
- catalog-backed capability overrides
- dynamic local-fleet projection for MLX/other config-driven providers
- planned-vs-supported model lifecycle markers once we begin carrying future provider families in the same graph
