---
title: Model Reflex — Per-Agent × Per-Ask × Per-Cognitive-Phase Routing
doc_type: proposal
domain: tooling-execution
status: proposed
last_updated: 2026-07-10
tags:
- models
- routing
- model-bindings
- cognitive-phase
- per-ask
- reflex
- precedence
related_docs:
- MODEL_GRAPH_FLYWHEEL_PROPOSAL.md
- ARCHITECTURE.md
task_refs:
- docs/task.md
proposal_id: model-reflex-routing
supersedes: []
implements: []
implemented_by: []
active_seams:
- model-reflex-per-ask
- model-reflex-cognitive-phase
- model-reflex-precedence-resolver
depends_on:
- per-agent-model-binding (Layer 1, PR #218)
related_proposals:
- model-graph-flywheel
source_of_truth_targets:
- ARCHITECTURE.md
---

# Model Reflex — Per-Agent × Per-Ask × Per-Cognitive-Phase Routing

## Goal

Give the operator (and, later, the routing oracle) deterministic, declarative control over **which model name executes each dispatch**, along three composable axes:

1. **per-agent** — the agent/role's own model preferences (shipped as Layer 1, PR #218: `TurnLoopConfig.model_bindings`).
2. **per-ask** — a single request may pin its own model, overriding the agent default for that dispatch only (e.g. a reflex or a caller that knows this particular ask needs a stronger model).
3. **per-cognitive-phase** — the *planning* leg of a turn (tool/reasoning re-entry loop) and the *responding* leg (final user-facing emit) may bind different models, so an agent can plan on a cheap/fast model and respond on a stronger one (or vice versa).

These are the operator's stated ultimate goal for model routing. This proposal is the **deterministic, operator-declarative** counterpart to the learned `model-graph-flywheel` routing oracle: the oracle *decides* a model; the reflex layer is *where that decision lands and is overridden by explicit operator intent*. They compose (see "Relationship to the routing oracle").

## Context: what Layer 1 already built

Layer 1 (PR #218) did two things that make Layer 2 a resolver change rather than a rewrite:

- **Funneled every dispatch through one resolver.** Every text-generation dispatch site now sets `ModelRequestPayload.model` from `role_model_binding(state, target_role)`, where `target_role` is whatever provider role `resolve_model_execution_target` picked (primary or fallback tier). model-router already reads `task.model`; `None` falls through to the provider's global default. Adding a new precedence tier means changing *what feeds that one field*, not touching model-router or the providers.
- **Physically separated the dispatch sites by phase.** The dispatch paths already distinguish the model-loop **re-entry** (tool/reasoning loop) from the **final emit** in both `runtime.rs` and `turn_loop.rs`, plus the per-tier fallback advance. Cognitive phase is *already* distinguishable at the call sites; Layer 2 only has to tag them and key bindings on the tag.

The base precedence Layer 1 documented is:

> explicit caller-set model (if any) > role binding for the resolved provider role > provider global default

but note the first tier is **stated intent, not yet enforced**: today the dispatch sites do `model_req.model = role_model_binding(...)`, an *unconditional overwrite*. Nothing sets a caller model before those assignments run, so it is harmless in Layer 1 — but per-ask is precisely the seam of making those assignments *preserve* an already-set caller/reflex model.

## Design: one resolver, four-tier precedence

All three axes collapse into a single resolution point that produces the model name for a dispatch. Proposed precedence, highest wins:

| Tier | Axis | Source |
|---|---|---|
| 1 | **per-ask** | model pinned on this specific request (caller/reflex-set on `ModelRequestPayload.model`, or a request-scoped override field) |
| 2 | **per-cognitive-phase** | the active role's phase-specific binding for `(phase, provider_role)` |
| 3 | **per-agent** | the active role's `model_bindings[provider_role]` (Layer 1) |
| 4 | **provider default** | `openrouter_default_model` etc. (`model` is `None`) |

### Seam A — per-ask (enforce the reserved top tier)

Change the dispatch-site assignments from unconditional overwrite to preserve-if-set:

```rust
// today (Layer 1): unconditionally overwrites
model_req.model = role_model_binding(state, &target_role);

// Layer 2: caller/reflex pin wins
model_req.model = model_req.model
    .take()
    .or_else(|| phase_or_role_binding(state, phase, &target_role));
```

This is a mechanical change at the same ~10 dispatch sites Layer 1 already isolated. A reflex or caller sets `ModelRequestPayload.model` (or a dedicated `model_override`) before dispatch, and it survives. No model-router change.

### Seam B — per-cognitive-phase

Tag each dispatch with its phase. A minimal taxonomy to start:

- **`plan`** — model-loop re-entry (the tool/reasoning loop; `request_class: "cognitive"` already marks these).
- **`respond`** — the final user-facing emit.

(Room to grow: `classify`, `summarize`, `route` — but ship `plan`/`respond` first; they map 1:1 onto the already-separated call sites.)

`role_model_binding` grows a phase-aware sibling that consults the phase binding first, then falls back to the phase-agnostic `model_bindings` (Layer 1), then the provider default. Because the call sites are already phase-distinct, tagging is a local edit per site, not new control flow.

### Open design question — how phase enters the data model

`TurnLoopConfig.model_bindings` is a `BTreeMap<provider_role, model_name>`, chosen as `BTreeMap` (not `HashMap`) specifically because **durability diffing** (mesh-config preserve-or-source, `seed_orchestrator_roles`) depends on deterministic serialization order. Any phase representation MUST preserve that constraint. Three candidates, with tradeoffs:

1. **Nested** — `BTreeMap<phase, BTreeMap<provider_role, model_name>>`.
   - Pro: clean separation; phase-agnostic bindings live under a reserved `"*"`/`"any"` phase or the existing flat map; easy to reason about precedence.
   - Con: two-level lookup; must define how nested and the existing flat `model_bindings` interact (recommend: flat map = the `any` phase).
2. **Compound key** — `BTreeMap<String, String>` keyed `"{phase}:{provider_role}"` (with bare `provider_role` = any-phase).
   - Pro: reuses the existing field/type verbatim — zero migration, no new serde surface, deterministic ordering for free.
   - Con: stringly-typed; parsing discipline; easy to fat-finger a key.
3. **Parallel map** — keep `model_bindings` (any-phase) and add `phase_model_bindings: BTreeMap<phase, BTreeMap<role, model>>`.
   - Pro: Layer 1 field untouched → strongest back-compat; phase feature is purely additive and independently `skip_serializing_if` empty.
   - Con: two fields to keep coherent in `role.configure` preserve-on-None mirroring.

**Recommendation:** option 3 (parallel map) for the cleanest back-compat with already-persisted Layer 1 role records, with the flat `model_bindings` remaining the any-phase base tier. Decide before implementation.

## Back-compat and durability

- All new fields `#[serde(default, skip_serializing_if = ...empty/None)]` so role records persisted by Layer 1 (or earlier) deserialize unchanged.
- `role.create_or_update` / `role.configure` extend the **preserve-on-None mirroring** already used for `fallback_tiers` and `model_bindings`: omitting a phase-binding field never clears it; pass an explicit value only to replace.
- Deterministic (`BTreeMap`) ordering preserved end-to-end so mesh-config durability diffing stays stable.

## Relationship to the routing oracle (`model-graph-flywheel`)

The flywheel's routing oracle *learns and decides* a model from operational signals. The reflex layer is the **execution and override plane** beneath it:

- The oracle can populate per-phase / per-agent bindings as its output, and the reflex resolver enforces them deterministically.
- Operator-set explicit bindings (any tier) outrank the oracle, giving a human-authoritative override the oracle cannot silently overturn — the same "explicit pin beats implicit default" principle Layer 1 established for the ladder (`explicit_pin`).
- Per-ask is the natural home for a shadow-mode oracle: log what the oracle *would* pick vs. what the reflex layer *did*, without touching the hot path, before granting the oracle primary-tier authority (see the flywheel's `model-oracle-primary-authority` follow-on seam).

## Test plan (extends the Layer 1 suite)

- **Per-ask precedence**: a request with `model` already set survives dispatch; role/phase bindings do NOT overwrite it; unset falls through to phase → agent → provider default.
- **Phase routing**: an agent with distinct `plan` vs `respond` bindings dispatches the plan (re-entry) leg on the plan model and the final emit on the respond model — asserted at the call sites, mirroring the Aria-isolation test's per-tier proof.
- **Composition**: per-ask > phase > agent > provider default, exercised as a single ordered table test.
- **Back-compat**: a Layer 1 role record (flat `model_bindings`, no phase field) still resolves correctly as the any-phase base tier.
- **Durability**: round-trip a role with phase bindings through mesh-config preserve-or-source; assert stable serialization ordering and no spurious diff.

## Seams

- `model-reflex-per-ask` — enforce the reserved top precedence tier (preserve caller/reflex-set model at the dispatch sites).
- `model-reflex-cognitive-phase` — phase taxonomy (`plan`/`respond`), phase-aware binding lookup, data-model decision.
- `model-reflex-precedence-resolver` — unify the four tiers behind one phase-aware resolver feeding `ModelRequestPayload.model`.

## Non-goals

- No model-router / provider changes — model-router already honors `task.model`.
- No learned/automatic selection — that is the flywheel oracle; this layer is deterministic and operator-declarative (and the surface the oracle writes into).
- No new dispatch call sites — Layer 2 changes what feeds the single resolved `model` field, reusing the sites Layer 1 isolated.
