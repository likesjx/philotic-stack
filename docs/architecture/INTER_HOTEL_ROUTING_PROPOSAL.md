---
title: "Inter-Hotel Routing Proposal"
doc_type: proposal
domain: mesh-placement
status: implemented
last_updated: 2026-03-12
tags:
  - routing
  - mesh
  - execution-plane
  - placement
  - source-of-truth
related_docs:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
  - NATIVE_OVERLAY_VPN_PROPOSAL.md
  - MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: inter-hotel-routing
implements: []
implemented_by:
  - execution-plane-routing-slice
active_seams:
  - placement-policy-broadening
  - multi-host-watched-validation
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
---

# Inter-Hotel Routing Proposal

## Goal

Extend Philotic's existing component routing model across hotel boundaries so local and remote execution use one contract instead of separate rulebooks.

## Core Recommendation

- Keep routing capability-first and reuse the same route vocabulary already used inside a hotel:
  - `target_node`
  - `target_role`
  - optional pinned `incarnation_id`
  - optional preferred hotel/environment hints
- Treat the mesh as a transport plane, not as a second routing abstraction.
- Split the transport plane into:
  - a UDP control plane for capability gossip, liveness, and lightweight coordination
  - a point-to-point data plane for routed execution traffic once a destination hotel is chosen
- Make each hotel authoritative for the live incarnations it materializes and advertises.
- Let unpinned remote routing resolve by deterministic placement scoring rather than by first-match selection.

## Disposition

`implemented`

## Current Slice

This proposal closes the architecture decisions needed before building a distributed capability and placement plane across hotels.

Accepted in this slice:
- inter-hotel routing should extend the existing intra-hotel route contract
- remote hotels should advertise capabilities and live incarnations
- incarnation identity should be namespaced by hotel authority
- unpinned remote selection should use deterministic placement scoring
- control-plane gossip and data-plane execution are explicitly different transport concerns
- first heartbeat/registry advertisement shape now exists for hotel-scoped incarnations with availability and placement hints
- heartbeat refresh and registry freshness filtering now exist for the first advertisement plane slice
- the hotel now exposes a live mesh-registry view and can route unpinned remote tool capabilities from that registry when no local runner is available
- the hotel now resolves model capability routes from the same registry-backed placement plane for `text.generate` and `media.analyze`, while membrane reply delivery remains session-owned rather than placement-selected
- routed execution now uses a first point-to-point TCP execution plane for inter-hotel `MESH_EVENT_BATCH` delivery instead of trying to cram rich tasks into UDP packets
- the first two-hotel remote model roundtrip is now smoke-green in local development over that TCP execution plane
- heartbeat and registry snapshots now include node-level execution reachability, and the dispatcher prefers those advertised execution endpoints before falling back to local loopback assumptions

Deferred from this slice:
- broader placement-based route selection beyond the first tool-capability fallback
- trust weighting and policy classes beyond basic eligibility
- full watched-live multi-host validation
- multi-transport tower negotiation beyond the first TCP execution plane

Linked work surface: [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)

## Routing Contract

The canonical route shape should stay shared between local and remote execution:

- `target_node`
- `target_role`
- optional `incarnation_id`
- optional `preferred_hotel_id`
- optional `preferred_environment_id`
- `selection_reason`

Interpretation:

- `target_role` or capability selects the execution class
- `incarnation_id` pins a concrete routed instance
- `target_node` identifies the hotel/node that owns the execution
- preferred hotel/environment steer selection when the route is not pinned

## Transport Separation

Routing and transport should stay distinct.

Control plane responsibilities:

- heartbeats
- capability advertisements
- liveness and pulse
- placement hints
- compact coordination and acknowledgments

Data plane responsibilities:

- task invocation
- task result delivery
- streaming progress and partial output
- larger payload references and artifact exchange

Current implementation note:

- current control-plane gossip still uses UDP Beacon traffic
- current routed execution now uses a TCP point-to-point execution plane for `MESH_EVENT_BATCH` delivery once a destination hotel is chosen
- the latest two-hotel remote model smoke is green on that TCP path after the previous UDP attempt failed with `Message too long (os error 40)`
- this is strong evidence that routed execution belongs on point-to-point reliable channels while UDP remains the control-plane substrate

## Authority Model

- A hotel is authoritative for the incarnations it materializes.
- Other hotels may cache advertisements, but they do not mint, rename, or override remote incarnation identity.
- Replies should route back through the owning route/session binding rather than being re-placed opportunistically.

## Incarnation Identity

Incarnation identity should be deterministic and hotel-scoped:

`incarnation_id = <hotel_name>:<guest_id>`

Carry both:

- `hotel_id` as the stable authority label
- `node_id` as the transport/runtime address identity

This keeps operator-facing identity legible while preserving a distinct transport address.

## Capability Advertisement

Hotels should advertise at least:

- `hotel_id`
- `node_id`
- `incarnation_id`
- `target_role` or capability
- `availability_state`
- `selection_hint`
- `latency_hint_ms`
- `max_concurrent_jobs`
- current load signal such as active jobs, queue depth, or normalized utilization
- node-level execution reachability for the current execution plane

Current implementation note:

- the first heartbeat/registry payload carries hotel id, node id, incarnation id, target role, availability state, selection hint, latency hint, max concurrency, active jobs, and queue depth
- the current builder derives local advertisements from active guest manifests and current live PID state
- hotels now emit periodic heartbeats to discovered peers and stale registry entries age out by TTL when queried
- `IpcServer` now exposes a live `__mesh_registry__` snapshot
- heartbeat payloads and registry snapshots now carry node-level execution reachability (`protocol`, `host`, `port`) for the first TCP execution plane
- current routing consumers now include:
  - tool assembly fallback: when no local runner exists for an unpinned tool capability, the hotel may choose a live remote advertisement using preferred hotel, lower latency, and higher available capacity before deterministic incarnation-id tiebreaking
  - model component route assembly: when no live local model implementation exists for `text.generate` or `media.analyze`, the hotel may choose a live remote advertisement using the same preferred-hotel, latency, capacity, and deterministic-id ordering
  - membrane reply delivery is intentionally excluded from placement selection because reply transport is bound to the session-owning membrane
  - outbound inter-hotel dispatch: when sending a routed task to a remote node, the dispatcher now prefers live execution reachability learned from heartbeat/registry and only falls back to `127.0.0.1:<execution_port>` for local multi-hotel development

## Placement Policy

When the route is not pinned:

1. pinned incarnation wins if present
2. preferred hotel/environment hints influence ordering
3. local viable candidate may win if local-first policy applies
4. otherwise choose the best eligible remote candidate by placement score
5. break ties deterministically by canonical `incarnation_id`

First-slice placement score inputs:

- latency
- available capacity / CPU headroom

Later additions may include:

- queue depth
- trust level
- cost class
- thermal or battery constraints

## ACK Boundary

Intended architecture:

- `MESH_EVENT_ACK` should mean the receiving hotel durably committed the inbound batch

Transitional current behavior:

- ACK may be emitted after enqueueing the inbound write rather than after strict post-commit confirmation

This should be kept explicitly transitional rather than quietly treated as final truth.

## Open Questions For Later Slices

- how capability advertisements are encoded and refreshed
- whether live load should be normalized as one score or sent as separate metrics
- how trust/policy classes influence placement
- how remote pinned guest specificity should behave when an incarnation is unavailable
