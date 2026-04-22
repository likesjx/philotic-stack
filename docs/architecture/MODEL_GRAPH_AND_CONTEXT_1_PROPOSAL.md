---
title: Model Graph And Context-1 Lookup Proposal
doc_type: proposal
domain: tooling-execution
status: proposed
last_updated: 2026-04-10
tags:
- models
- routing
- approvals
- graph
- benchmarks
- context-lookup
- llm-stats
related_docs:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
- MODEL_CONTROLLER_PROPOSAL.md
- TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md
- APPROVAL_UX_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: model-graph-context-1
implements: []
implemented_by: []
active_seams:
- model-graph-decision-layer
- context-1-lookup
- capability-aware-tool-approval
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Model Graph And Context-1 Lookup Proposal

## Goal

Create a weighted model graph that helps Philotic choose which model should handle a turn or tool decision, based on real capability signals instead of hand-wavy "this one feels smart" folklore.

The graph should also expose a fast `context-1` lookup: a shallow, explainable query that returns the best model candidates for the current task plus the key reasons they were ranked where they were.

The point is not to replace `model-router` or session approval policy. The point is to give those existing seams a better advisor than vibes in a trench coat.

## Core Recommendation

Philotic should treat model selection as a graph-backed decision problem with three distinct layers:

1. Observed model facts - benchmark results, context windows, pricing, latency, and modality support
2. Operational fit - how well a model matches the current request class, tool risk, and turn shape
3. Runtime policy - what the current session, role, and approval rules allow

The weighted graph should live above provider invocation and above human approval enforcement:

- `model-router` remains the provider execution boundary
- `philote` remains the runtime approval boundary
- the model graph becomes the advisory selection layer in front of both

`llm-stats.com` should be one ingestion source for benchmark and pricing signals, not the only source of truth. Local observations, operator policy, and current runtime constraints must still override a shiny leaderboard when reality disagrees.

## Disposition

`proposed`

Track implementation in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Current Slice

Define the first honest slice that can make the model graph useful without turning it into another shadow control plane:

- map the current code paths that already do provider resolution and approval gating
- define the first graph schema for model facts, benchmark observations, and task-fit edges
- define the `context-1` lookup input/output contract
- define the approval advisory flow for tool calls without weakening hard approval policy
- keep the first implementation slice shallow enough that the runtime can explain its own answer

This slice does not require a full historical model warehouse, a training pipeline, or a universal benchmark importer.

## Current Repo Reality

The repo already has the important seams, even if it does not yet have a weighted model graph:

- `crates/model-router/src/controller.rs` resolves providers through `ProviderRegistry::resolve()`
- `crates/model-router/src/controller.rs` already carries `request_class`, `response_contract`, `context`, `affordances`, `routing_hints`, and `provider_options`
- `crates/philote/src/session.rs` already gates approval with `approval_policy_allows()`
- `crates/philote/src/runtime.rs` already resolves approval requests against the active tool call
- `crates/ansible-mesh-core/src/registry.rs` already stores capability advertisements with latency and concurrency hints
- `docs/task.md` already tracks provider-native response-mode routing and the current model-controller envelope work

So the real question is not "where do we invent a model graph?" It is "where do we attach graph-backed advice so the existing seams make better decisions?"

## Implementation Map

The first slice should land in this order:

1. Graph vocabulary
   - add model-oriented records to the shared graph layer
   - seed or ingest model facts from external sources and local observations
2. Lookup service
   - expose a `context-1` query that returns ranked candidate models for a task
   - keep the query shallow enough to be fast, deterministic, and explainable
3. Provider selection
   - feed the lookup result into `model-router` before provider fallback
   - preserve the existing provider hint and hard-fail behavior when no candidate is viable
4. Approval advisory
   - let `philote` consume the model-graph advisory when deciding whether a tool call is low-risk enough for preapproval-based bypass
   - keep hard approval policy as the final authority

That order matters. If approval is changed before model selection is explainable, the graph becomes a covert policy engine with better stationery.

## Proposed Graph Schema

The graph should separate stable model identity from ephemeral observations.

### Core nodes

- `model_profile`
  - canonical model identity, provider, family, version, and modality support
- `model_benchmark_observation`
  - a time-stamped score from an external benchmark or local evaluation
- `model_operational_signal`
  - pricing, latency, context-window, and trust metadata from live runtime use
- `model_task_fit`
  - task-specific fit edge between a model and a capability/request class
- `model_policy_signal`
  - policy or trust labels that influence whether a model is appropriate for a given task

### Suggested model fields

- `model_ref`
- `provider`
- `family`
- `version`
- `modalities`
- `max_context_tokens`
- `input_cost_per_million`
- `output_cost_per_million`
- `latency_hint_ms`
- `tool_use_quality`
- `reasoning_quality`
- `multimodal_quality`
- `trust_class`
- `source`
- `observed_at`

### Suggested edges

- `supports_modality`
- `supports_request_class`
- `observed_via`
- `derived_from`
- `better_for`
- `weaker_for`
- `blocked_by_policy`
- `preferred_for`

This is intentionally more boring than a giant ontology. Boring is good here. A model graph should explain itself instead of wearing a cape.

## Context-1 Lookup

`context-1` is the shallow query path that asks: "What model should handle this decision right now?"

### Inputs

- `task_kind`
- `request_class`
- `required_modalities`
- `tool_name` or `tool_class`
- `risk_class`
- `context_window_needed`
- `latency_slo_ms`
- `budget_hint`
- `policy_constraints`
- `provider_hint`

### Outputs

- `selected_model`
- `top_alternatives`
- `scores`
- `reasons`
- `hard_filters_applied`
- `approval_risk_hint`
- `confidence`

### Intended behavior

- one hop, not a graph crawl
- explainable, not magical
- quick enough to run on the critical path
- stable enough to memoize if the exact same turn shape repeats

If the lookup cannot explain its choice, it should not silently pretend to be wise.

## Scoring Model

Use a weighted score with hard filters first.

### Hard filters

Reject a candidate when any of these fail:

- required modality unsupported
- required context window too small
- provider or trust class blocked by policy
- task type unsupported
- model is stale or missing required operational data

### Weighted signals

After hard filters, rank remaining candidates with weighted inputs such as:

- task/capability fit
- benchmark quality
- tool-call quality
- context-window headroom
- latency
- cost
- trust/policy fit
- freshness of the signal

### Suggested first weights

- `task_fit`: 0.30
- `benchmark_quality`: 0.20
- `tool_quality`: 0.15
- `context_headroom`: 0.10
- `latency`: 0.10
- `cost`: 0.05
- `trust_policy`: 0.05
- `signal_freshness`: 0.05

Those weights are intentionally just a starting point. If they become scripture, we will have re-invented dogma with a JSON schema.

## Approval Flow

Capability-aware approval should be advisory, not sovereign.

### Flow

1. A turn or tool decision arrives.
2. The `context-1` lookup ranks model candidates.
3. The best model is selected for the current task or tool advisory pass.
4. The selected model's capability and confidence are used as input to approval reasoning.
5. `philote` still applies the final approval policy.

### What model capability may influence

- whether a low-risk tool call can be auto-resolved from preapproval
- whether a second-pass model review is needed before surfacing approval to the operator
- whether a tool request should be re-routed to a more capable or more trustworthy model before asking for human intervention

### What model capability may not override

- forbidden tools
- explicit operator denial
- class-level hard approval policy
- any runtime security rule that already acts as final authority

This keeps the graph useful without turning it into an approval oracle with a leaderboard fetish.

## First Slice Recommendation

The smallest useful slice should be:

- a model graph record type and storage path
- a shallow `context-1` lookup API
- a first provider-selection use of that lookup in `model-router`
- a read-only advisory hook in `philote` approval reasoning

The first slice should not try to solve:

- model fine-tuning
- training pipelines
- automatic benchmark crawling for every provider on earth
- policy replacement
- long-horizon reinforcement learning

Those can be later slices if the first one proves the shape.

## Open Questions

- Should the graph use one canonical `model_profile` record plus time-stamped observations, or separate records per provider endpoint?
- Should `context-1` live in `model-router`, in the graph service, or as a shared lookup helper?
- Should the approval advisory return a simple confidence threshold or a richer reason code set?
- Which benchmark families should count as first-class inputs for local model fit versus merely informational context?
- Should we treat capability confidence as a multiplier on approval risk, or only as a router hint?
