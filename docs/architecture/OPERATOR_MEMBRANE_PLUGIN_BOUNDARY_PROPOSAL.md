---
title: "Operator Membrane Plugin Boundary Proposal"
doc_type: proposal
domain: membrane-transport
status: proposed
last_updated: 2026-03-20
tags:
  - membrane
  - operator-surface
  - desktop
  - plugin-boundary
  - control-plane
related_docs:
  - ARCHITECTURE_STATUS.md
  - DESKTOP_MEMBRANE_PROPOSAL.md
  - MEMBRANE_COMPONENT_PROPOSAL.md
  - CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md
  - PHILOTIC_WEB_PROPOSAL.md
  - ROUTED_OPERATOR_CHAT_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: operator-membrane-plugin-boundary
implements:
  - membrane-component
implemented_by: []
active_seams:
  - desktop-membrane-boundary
  - operator-membrane-plugin-boundary
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
---

# Operator Membrane Plugin Boundary Proposal

## Goal

Define the plug-and-play boundary between `aiua` and operator-facing membranes so desktop/operator features can evolve without repeatedly editing daemon bootstrap, IPC switchboards, and shared runtime enums.

The immediate trigger is straightforward: the current desktop membrane work proved useful runtime seams, but the next ridge started pushing `desktop_membrane.*` query and chat behavior into `aiua/src/main.rs`, `aiua/src/service/ipc.rs`, and shared client contracts. That is a good prototype smell and a bad destination.

The direction is now sharper:

- operator surfaces should be reusable by membranes, agents, and automation
- they should behave like general functional intercept planes rather than desktop-only endpoints
- the router should be the canonical handoff mechanism between caller intent and target execution

## Core Recommendation

Treat the operator membrane as a **replaceable component adapter** over a **stable operator control-plane contract**, not as special-case daemon behavior.

Treat operator surfaces as **general functional intercept planes** that can be queried by:

- desktop membranes
- web membranes
- CLI/admin tooling
- agents
- automation/workflow components

Recommended split:

1. `aiua` owns generic authority and transport primitives:
   - lease issuance and renewal
   - task/event routing
   - session persistence
   - node registry visibility
   - generic operator surface execution hooks
   - target-hotel validation and audit authority
   - router-mediated handoff semantics
2. operator membranes own operator-facing features:
   - shaped inventory/status/session/agent views
   - operator UX flows
   - target selection and attribution presentation
   - membrane-specific chat affordances
   - HTTP/websocket/browser delivery details
3. the seam between them should be an operator control-plane contract that is:
   - membrane-agnostic
   - versionable
   - auditable
   - reusable by desktop, web, CLI, or future membranes
4. non-local execution and cross-authority delivery should hand off through the router plane rather than through bespoke membrane-specific query choreography

Put more bluntly: `desktop_membrane.query_agents.v1` living in daemon bootstrap is the architectural equivalent of “temporary” duct tape discovering tenure.

## Disposition

`accepted for current slice`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Current Slice

This slice is a **boundary-correction pause**:

- keep the already-landed desktop membrane hardening and hotel-owned read-model work
- do **not** continue landing remote agent inventory or membrane chat by adding more desktop-specific branches in `aiua` core
- extract the next contract shape before adding new operator surface

This means the following stay as current truth:

- desktop membrane lease lifecycle
- same-origin cookie bootstrap
- hotel-owned local `status`, `guests`, and redacted `agents`
- mesh target inventory
- target status query/fallback
- target guest query/fallback
- apartment denial by default

This means the following are intentionally paused:

- remote agent inventory as a daemon-owned desktop query specialization
- membrane-native chat flows implemented directly inside `aiua` bootstrap/IPC switchboards

This slice now goes one step further and defines the first concrete reusable operator surface family plus the router handoff envelope that future code should target.

It also now has a first implementation foothold:

- shared IPC contracts now expose generic `QueryOperatorTargets`, `QueryOperatorTargetStatus`, and `QueryOperatorTargetGuests` requests
- shared IPC contracts now expose generic `QueryOperatorTargetAgents` as the bounded redacted target-agent inventory surface
- `aiua` serves those generic requests through the same hotel-owned target logic already proven by the desktop membrane
- `philotic-web` now calls the generic operator target requests and keeps the current desktop routes as adapters
- the daemon-owned remote query worker now uses a typed shared `OperatorSurfaceQueryHandoff` envelope plus generic `management.operator_surface_query` role names for routed target status and guest queries
- the same generic routed handoff path now also serves `operator.targets.agents`, so remote agent inventory no longer needs a fresh desktop-specific query family
- shared target payload structs are now operator-owned in `philotic-client`, with `DesktopMembraneTarget*` names retained only as compatibility aliases for the current adapter layer

The current acceptable transitional adapters are now explicit:

- the desktop HTTP route shapes in `philotic-web` for `/api/mesh/targets`, `/api/mesh/targets/:target_node_id/status`, and `/api/mesh/targets/:target_node_id/guests`
- the desktop-specific route naming itself, so long as those routes are thin adapters over generic operator target requests

The current core-boundary drift that should not spread further is also explicit:

- adding new desktop-specific request or response variants to shared IPC when a generic operator surface name would do
- adding new desktop-specific worker roles or action families in daemon bootstrap
- introducing browser- or desktop-shaped payload semantics into router handoff execution
- building operator chat or remote agent inventory directly on new `desktop_membrane.*` contracts

The next intended seam for operator chat is now tracked in [ROUTED_OPERATOR_CHAT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROUTED_OPERATOR_CHAT_PROPOSAL.md): desktop operator chat should be a membrane ingress into the same routed agent conversation plane used by Telegram, not a separate admin RPC family.

## Boundary Rules

### What stays in `aiua`

`aiua` may own:

- lease registries and authority checks
- generic `EmitTask` / `CreateTask` / session recording behavior
- local and remote event/task transport
- generic operator management inboxes
- node registry observation
- target-hotel canonical reads and writes
- target-hotel route resolution for local guests and roles
- router-mediated handoff and delivery semantics

### What should move out of `aiua`

`aiua` should not accumulate:

- desktop-specific action names
- desktop-specific view-model structs
- browser-oriented request/response shapes
- membrane-specific UX flows
- desktop/operator chat orchestration logic

### What belongs in a membrane adapter layer

The membrane adapter layer should own:

- shaped operator view models
- feature-specific read aggregation
- membrane-facing API contracts
- chat/session UX semantics for operators
- compatibility shims for desktop/web delivery

That adapter may live in:

- a dedicated membrane crate
- a philotic-web operator-control-plane adapter module
- or a future generic operator membrane service

But the important rule is ownership, not folder aesthetics.

## Proposed Extraction Shape

### Stable seam

Introduce a stable operator control-plane seam with concepts like:

- `operator.query_targets`
- `operator.query_target_status`
- `operator.query_target_guests`
- `operator.query_target_agents`
- `operator.start_agent_turn`
- `operator.observe_turn`

Those names are examples, not canon. The important part is that they are operator-control-plane concepts, not desktop implementation details.

These surface planes should support:

- caller identity
- posture/grant scope
- redaction level
- denial or partial-visibility explanation
- router handoff metadata when execution leaves the local authority boundary

## First Surface Family

The first concrete reusable operator surface family should be target-oriented and intentionally modest:

- `operator.targets.list`
- `operator.targets.status`
- `operator.targets.guests`
- `operator.targets.agents`

These four surfaces are enough to prove:

- reusable operator-facing visibility
- caller-aware redaction
- local-versus-routed fulfillment
- router-mediated handoff for non-local reads

They are also the exact family the current desktop membrane already wants, which is convenient in the non-sinister way for once.

### Surface semantics

`operator.targets.list`

- returns the visible target inventory for the caller
- source-of-truth may be local registry plus local authority-owned attribution
- may be satisfied locally when the local hotel owns the registry view

`operator.targets.status`

- returns canonical local target status when the target is local
- routes through the router handoff plane when the target is remote
- may return fallback observation only when the contract explicitly says the response is observational rather than canonical

`operator.targets.guests`

- returns canonical local guest inventory when the target is local
- routes through the router handoff plane when the target is remote
- must not fabricate remote guest truth from registry gossip

`operator.targets.agents`

- returns canonical redacted local agent inventory when the target is local
- routes through the router handoff plane when the target is remote
- should be the first surface to prove caller-aware redaction rather than desktop-only shaping

### Shared response envelope

Each surface should project through a shared envelope with fields along these lines:

- `surface`
- `source_hotel`
- `target_hotel`
- `target_node_id`
- `fulfillment_kind`
- `visibility_scope`
- `available`
- `pending_state`
- `data`
- optional `denial`
- optional `freshness`
- optional `handoff`

Where:

- `fulfillment_kind` distinguishes `local-canonical`, `routed-canonical`, and explicit observational fallback states
- `visibility_scope` captures the caller-appropriate projection level
- `pending_state` is how we avoid silent hand-waving when a routed query is in-flight, unavailable, or denied

## Caller-Aware Projection Rule

These surfaces are shared across:

- desktop membranes
- agents
- automation
- admin tooling

So the response shape should stay stable while the projection may vary by caller posture.

That means the contract should support:

- same surface name
- same envelope shape
- different `visibility_scope`
- different redaction level
- explicit denial payload when the caller is not entitled to more

Recommended initial visibility scopes:

- `operator_full`
- `operator_limited`
- `agent_admin`
- `agent_limited`
- `automation_scoped`

Those names can change, but the rule should not: one surface family, multiple caller projections, no parallel secret API just because the desktop showed up first.

### Adapter responsibilities

Then let `philotic-web` or another membrane adapter map:

- HTTP routes
- websocket/session behavior
- desktop UI expectations
- target selection UX

onto that seam.

### Runtime hook shape

`aiua` should expose one generic operator management execution hook or inbox, not an expanding family of `desktop_membrane.query_*` branches.

That generic hook can still:

- validate the caller
- run hotel-owned canonical reads
- hand off to the router for remote execution
- preserve audit trails

But the payload family should describe operator actions generically enough that another membrane can reuse them.

## Router As Handoff Mechanism

The router should be the canonical handoff mechanism for operator surfaces that cross authority, placement, or execution boundaries.

That means:

- membranes do not handcraft remote hotel query choreography as their own private transport ritual
- agents do not need a second privileged path just to inspect operator-visible surfaces
- remote operator actions and remote operator reads can share one routed handoff story

Recommended rule:

- if a surface can be answered canonically and locally, the local authority may answer directly
- if a surface requires another hotel, guest, or execution context, the request should be handed to the router plane with explicit source, target, and intent metadata

The router therefore becomes the shared handoff layer for:

- membrane -> target hotel
- agent -> operator surface
- automation -> operator surface
- local authority -> remote authority

## First Router Handoff Envelope

The first reusable router handoff envelope for operator surfaces should be explicit and transport-agnostic.

Suggested fields:

- `handoff_kind`
- `surface`
- `request_id`
- `source_hotel`
- `target_hotel`
- `target_node_id`
- `caller_kind`
- `caller_id`
- `visibility_scope`
- `grant_scope`
- `intent`
- `payload`
- optional `session_id`
- optional `trace`

Recommended initial semantics:

- `handoff_kind = "operator_surface_query"`
- `surface` names one of the reusable surface planes
- `intent` explains what is being requested without smuggling desktop semantics into the router
- `payload` carries surface-specific parameters such as target selectors

This envelope is for handoff, not for browser delivery. Membranes may adapt it, but should not own it.

### Handoff rule

Use the router handoff envelope when:

- the surface cannot be fulfilled canonically by the local authority
- the target authority is another hotel
- the request needs explicit routed delivery semantics

Do not use the router handoff envelope when:

- the answer is local and canonical already
- the request is pure presentation-layer reshaping

The goal is not to route everything out of aesthetic devotion. The goal is to route the cross-authority seams and stop every membrane from becoming its own little shipping department.

## Why This Matters

Without this split, “plug and play membrane” quietly becomes:

- one official membrane baked into the daemon
- one growing IPC enum for that membrane
- one bootstrap worker per membrane feature family

That is not plug and play. That is a favored plugin welded into the chassis.

## Next Slice

The next implementation slice should be:

1. define the generic operator control-plane contract as reusable operator surface planes
2. identify which already-landed desktop membrane shapes can remain transitional adapters
3. define router-mediated handoff rules for non-local operator surface execution
4. lift the first target-oriented surface family onto that contract
5. extract desktop-specific naming and behavior out of daemon bootstrap
6. only then resume remote agent inventory and operator chat on top of the extracted seam

## Reality Gap

The repo now has proven membrane hardening value, but the plug-and-play boundary is not yet proven.

What is proven:

- hotel-owned authority and read-model shaping are the right direction
- lease-backed membrane lifecycle works
- remote target reads can ride explicit management-plane queries

What is not yet proven:

- that operator membranes can be added or swapped without editing daemon core
- that operator chat can be delivered through a membrane-agnostic control-plane seam
- that the router can act as the shared handoff mechanism for operator surface execution instead of each membrane inventing its own transport ritual
