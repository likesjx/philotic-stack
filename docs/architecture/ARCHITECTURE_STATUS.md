---
title: "Philotic Architecture Status"
doc_type: status
domain: runtime-sessions
status: active
last_updated: 2026-03-19
tags:
  - source-of-truth
  - current-state
  - active-seam
  - transitional
related_docs:
  - README.md
  - ARCHITECTURE.md
  - SESSION_LOOP_PROPOSAL.md
  - TELEGRAM_POLL_LEASE_PROPOSAL.md
  - DESKTOP_MEMBRANE_PROPOSAL.md
  - OPERATOR_MEMBRANE_PLUGIN_BOUNDARY_PROPOSAL.md
  - RUNTIME_AUTHORITY_LEASES_PROPOSAL.md
  - MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md
  - DOC_TAGGING_FRONTMATTER_PROPOSAL.md
task_refs:
  - docs/task.md
tracks_domains:
  - runtime-sessions
  - membrane-transport
  - mesh-placement
  - memory-context
  - tooling-execution
  - operator-control-plane
  - deployment-distribution
  - migration-parity
---

# Philotic Architecture Status

> **Status:** Living Snapshot | **Last Updated:** 2026-03-19

This document is the single source of truth for Philotic's current architecture status.

Use it to answer three questions fast:

1. What is implemented and considered current repo truth?
2. What is intentionally transitional?
3. What is actively being worked right now?

This is not the place for full design arguments. For those, follow the linked proposal docs.

## How To Read This

- `Implemented` means there is code and test evidence in the repo today.
- `Transitional` means the shape is real enough to rely on for the current slice, but it is not presented as final architecture.
- `Active` means the seam is currently hot based on [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md), active proposals, and the observed worktree on 2026-03-12.
- when convenience docs disagree on concrete transport details, current code and [docs/README.md](/Users/jaredlikes/code/philotic-stack/docs/README.md) win over stale crate-level prose.

## Current Architecture Summary

Philotic currently operates as a hotel-centered runtime:

- `aiua` is the runtime authority for hotel orchestration, guest materialization, context-graph persistence, IPC handling, and inter-hotel coordination.
- `membrane`, `philote`, `model-router`, and `tool-runner` are separate guest-facing binaries with explicit runtime boundaries.
- local guest-to-hotel IPC currently runs over Unix domain sockets, driven by `PHILOTIC_HOTEL_SOCKET`; default paths include `/tmp/philotic-aiua.sock` for generic local clients and hotel-specific socket paths such as `/tmp/philotic-<hotel>.sock` when `aiua` materializes a named hotel.
- canonical session state now lives in the context graph, while apartment-style checkpoints remain derived recovery projections rather than a competing source of truth.
- Telegram ingress is session-aware and guarded by hotel-owned poll-lease authority, with explicit delegated remote polling available as a transitional exception.
- local and remote execution routing both exist, but several placement, delegation, and admin/control-plane seams are still under active development.

## Implemented Foundations

### Runtime and authority

- one hotel daemon per machine is the current runtime model
- the context graph is the canonical durable owner for hotel and session state
- guest materialization and supervision are hotel-owned responsibilities
- guest binaries are resolved through the current binary-resolution contract rather than hardcoded `target/debug` assumptions

Primary references:
- [ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE.md)
- [GUEST_BINARY_RESOLUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/GUEST_BINARY_RESOLUTION_PROPOSAL.md)

### Sessions and approvals

- generalized session records, participants, turns, and events are modeled in the graph layer
- transport identities in `membrane` bind to stable `session_id` values
- session timeline/progress events persist through the IPC plane
- approval policy, preapproval, and session status/bindings are included in session snapshots
- approval interrupts and slash-command steering are implemented in the current agent loop

Primary references:
- [SESSION_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SESSION_LOOP_PROPOSAL.md)
- [AGENT_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_LOOP_PROPOSAL.md)
- [APPROVAL_UX_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/APPROVAL_UX_PROPOSAL.md)

### Membrane and Telegram

- Telegram text, photo, and voice ingress normalize into structured envelopes
- slash commands are short-circuited before the normal model path
- Telegram poll leases are hotel-owned, renewed, fenced, explicitly released on graceful shutdown, and can be delegated to named remote hotels as a transitional contract
- poll authority is anchored to the agent's home hotel rather than the current routed role

Primary references:
- [TELEGRAM_INTEGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_INTEGRATION_PROPOSAL.md)
- [TELEGRAM_POLL_LEASE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_POLL_LEASE_PROPOSAL.md)
- [SLASH_COMMANDS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SLASH_COMMANDS_PROPOSAL.md)

### Routing and execution

- the hotel advertises local capability availability and can route to remote execution advertisements when local implementations are unavailable
- inter-hotel execution transport is now distinct from raw UDP beacon payload bodies
- reply routing remains session-owned through the membrane boundary

Primary references:
- [INTER_HOTEL_ROUTING_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/INTER_HOTEL_ROUTING_PROPOSAL.md)
- [NATIVE_OVERLAY_VPN_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/NATIVE_OVERLAY_VPN_PROPOSAL.md)
- [MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md)

### Tooling and model execution

- abstract tool catalog seeding exists in the context graph
- tool assembly uses catalog-backed metadata and approval annotations
- local workspace tooling exists through `tool-runner`, although broader routed error-envelope and management-plane work remains incomplete
- `model-router` is the shared model execution boundary for current providers

Primary references:
- [TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md)
- [TASK_RUNNER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TASK_RUNNER_PROPOSAL.md)
- [MODEL_CONTROLLER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MODEL_CONTROLLER_PROPOSAL.md)

### Deployment and memory protocol

- the first VPS deployment boundary is defined with Red Hat Ansible as outer control plane and Philotic hotel runtime as inner authority
- Muninn bootstrap and required-memory-session discipline are part of the repo's active workflow contract

Primary references:
- [RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md)
- [MUNINN_MEMORY_PROTOCOL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MUNINN_MEMORY_PROTOCOL_PROPOSAL.md)
- [AGENT_WORKFLOW_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_WORKFLOW_PROPOSAL.md)

## Transitional Architecture

These are real current choices, but they are explicitly not the final story:

- Tailscale/MagicDNS remains the named transitional scaffold for deployed inter-hotel reachability
- model/provider egress is still an explicit exception rather than routed through a perimeter egress plane
- build-on-host VPS deployment is still transitional until artifact distribution hardens
- role incarnation design direction is adopted, and the first graph/routing substrate now exists; current design direction now favors context-shift role activation with shared self/memory by default, while concurrent role materialization remains conditional
- some architecture-facing crate READMEs still carry older `Ansible`, port-oriented, or socket-path wording and should be treated as convenience narratives until they are reconciled with current code

## Proposed Architecture Directions

These are accepted proposals not yet in implementation:

| Proposal | Core idea | Status |
| --- | --- | --- |
| Agent-centric resource model | Agents declare and request resources; hotel acts as broker; demand-derived materialization replaces static guest config; agent graph is a mesh-synced tool-runner resource; router-listener generates RL training traces | [AGENT_RESOURCE_MODEL_PROPOSAL.md](AGENT_RESOURCE_MODEL_PROPOSAL.md) — proposed |
| Graph layer unification | Introduce `GraphDomain` as the unified middle layer; all domain operations expressed in terms of `GraphAdapter` primitives; one update point for entity types across all graph stores | [GRAPH_LAYER_UNIFICATION_PROPOSAL.md](GRAPH_LAYER_UNIFICATION_PROPOSAL.md) — proposed |
| Architectural rules and roadmap | Extract standing constraints from proposals into ARCH_RULES.md; maintain dependency-ordered seam roadmap in ROADMAP.md; check rules at slice close-out | [ARCH_RULES_AND_ROADMAP_PROPOSAL.md](ARCH_RULES_AND_ROADMAP_PROPOSAL.md) — proposed |

## Active Work Right Now

These are the most clearly active seams as of 2026-03-13:

| Seam | Current truth | Next pressure |
| --- | --- | --- |
| Session leases and ownership semantics | session durability, approval state, and timeline projection exist; explicit active-work ownership semantics are still incomplete in the task board | define and implement canonical active ownership without creating a second authority shadow |
| Runtime authority leases | a shared `LeaseEnvelope` and central runtime lease registry/provider now exist, Telegram poll lease has been migrated onto that abstraction, and the first explicit boundary contract now separates lease authority from materialization, supervision, routing, and vault access | move the next runtime seam onto the shared provider path and prove the contract on a non-Telegram path |
| Desktop membrane boundary | `philotic-web serve` now acquires and renews a dedicated desktop membrane lease, serves the embedded desktop through a same-origin `HttpOnly` session cookie without JS credential injection, uses same-session cookie auth for both API and WebSocket access, routes the first membrane reads (`status`, `guests`, `agents`) through explicit hotel-owned IPC view models, exposes a first typed `/api/mesh/targets` inventory with `source_hotel`, `target_hotel`, reachability, and freshness attribution from the hotel-owned registry, adds `/api/mesh/targets/:target_node_id/status` with `local-canonical` local status plus a real remote management query attempt that falls back to `remote-heartbeat-observed` when the target does not answer, exposes `/api/mesh/targets/:target_node_id/guests` with canonical local reads plus a first real cross-hotel management query attempt backed by a daemon-owned generic `management.operator_surface_query` worker and typed `OperatorSurfaceQueryHandoff` envelope with explicit `remote-query-failed` fallback when the remote path cannot complete, exposes `/api/mesh/targets/:target_node_id/agents` as a bounded redacted target-agent inventory surface with canonical local reads and the same routed remote query/failure semantics, exposes `POST /api/mesh/targets/:target_node_id/agents/:agent_id/chat` as the thin desktop operator-chat adapter over the canonical conversation path, now returning `202 Accepted` and streaming in-flight `operator_chat:turn_event`, `operator_chat:partial_reply`, `operator_chat:reply`, and `operator_chat:error` updates over `/ws` while the routed turn is in flight; the synchronous operator-chat helper now preserves `partial_reply` frames as non-terminal observations instead of mistaking them for the final answer, the lower conversation/model path can now carry optional `partial_replies` through `model_result.result.partial_replies` so `philote` emits real `partial_reply` frames before the final reply, and the Telegram membrane now edits the active draft message on `partial_reply` / final text completion instead of treating progressive delivery as a decorative comment; targeted test proof shows a routed operator chat turn can leave the local hotel, traverse a remote-hotel bridge, and return to the local reply inbox over the same conversation semantics; apartment inspection remains explicitly denied on the default membrane surface; bearer auth remains only as a transitional remote/debug path; the first reusable `operator.targets.*` IPC contract is now landed for targets/status/guests/agents, the first routed operator-chat contract is landed, the shared target payload structs are operator-owned with desktop names retained only as compatibility aliases, `philotic-web` uses that seam as an adapter under the current desktop routes, and only the current target-oriented desktop routes are still accepted as transitional adapters | provider-native incremental generation, watched-live remote-hotel proof beyond the current test bridge, and backing-authority swaps behind router resolution |
| Governed workflow skills | first `abstract_skill` graph scaffolding now exists for handoff/governance, and the architecture now distinguishes same-agent role handoff from peer delegation and external cognitive peer handoff; same-identity handoff is now being refined toward context-shift semantics by default, and the existing `HandoffBundle` wire path now carries a first compatibility-first richer packet | skill lifecycle validation layers, field sourcing map, meta-skill contract, and `skill.register` + `subagent.spawn` tool contracts are now defined in `SKILL_LIFECYCLE_PROPOSAL.md`; first implementation slice covers data contracts, Layer 1 validation, hotel lease/hook registries, real `SpawnSubagent` execution, and `philote` split into persona + worker binaries |
| Telegram membrane authority | poll-lease acquire, renew, expiry, home-hotel checks, graceful release, dual-poller smoke coverage, and explicit delegated remote polling are implemented | canonical mesh-visible poll authority is still deferred |
| External membranes and edge trust | membrane is documented as the outside-world boundary, and `A2A` / `Nostr` are now proposed as membrane transports with explicit trust, sentinel, and perimeter-defense contracts rather than mesh replacements | define the first normalized external transport envelope, external principal trust records, and membrane sentinel finding schema before implementing one narrow transport |
| Mesh-visible state placement | current local authorities mostly live in hotel runtime state, SQLite, or file-backed records; shared criteria for what becomes mesh-visible are now being defined explicitly | classify current state families and stop solving each cross-hotel visibility seam with a bespoke projection ritual |
| Role incarnation model | `RoleIncarnationRecord`, `TurnLoopConfig`, `ConfigureRole` IPC action, session `active_incarnation_id`, inbound agent-task routing to the active incarnation, orchestrator fallback for missing active guests, a first parked-delivery/on-demand materialization path for configured inbound roles, basic `HandoffToRole` / `HandoffBack` IPC, `/role <name>` + `/back` + `/roles` operator surfaces, first `abstract_skill` graph scaffolding, a compatibility-first typed `role_activation` object through hotel snapshot -> `philote` session state -> context projection, a first richer same-identity handoff packet through the existing `HandoffBundle` path, and a compatibility-first `SubagentDelegation` / `SpawnSubagent` wire contract with explicit not-yet-implemented hotel rejection now exist; current design direction prefers shared-self role context shifts by default and delegated subagents for parallel labor, while worker lifecycle, role governance, and skill-layer behavior remain incomplete | expand `RoleActivation`, formalize workflow-owned handoff/delegation assembly rules, implement real `SpawnSubagent` execution and result routing, and only then decide where concurrent role materialization is genuinely warranted |
| Tool execution envelope | catalog-backed tools and approval policy exist | extend structured error behavior across more routed components instead of falling back to ad hoc strings |
| Perimeter egress control | proposal exists and the lack of a unified egress plane is explicitly called out | define the first policy object and classify current egress exceptions |
| Deployment hardening | VPS boundary and peer rendering contract are defined | remove plaintext secret assumptions and prove real VPS smokes |

## Domain Status Matrix

| Domain | Status | Source of truth | Active work |
| --- | --- | --- | --- |
| Runtime and sessions | implemented, still evolving | [SESSION_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SESSION_LOOP_PROPOSAL.md) and code in `aiua`, `philote`, `ansible-mesh-core` | session ownership semantics, compaction policy, bounded loop follow-through, and role context-shift semantics |
| Membrane and transport | implemented, still evolving | [TELEGRAM_INTEGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_INTEGRATION_PROPOSAL.md), [TELEGRAM_POLL_LEASE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_POLL_LEASE_PROPOSAL.md), [DESKTOP_MEMBRANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DESKTOP_MEMBRANE_PROPOSAL.md), and [MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md) | delegated poll authority, desktop/operator membrane hardening, broader transport surfaces, and external membrane trust/edge-defense contracts |
| Mesh and placement | partially implemented | [INTER_HOTEL_ROUTING_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/INTER_HOTEL_ROUTING_PROPOSAL.md), [MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md) | placement policy, trust boundaries, overlay evolution, and mesh-visible state classification |
| Memory and context | partially implemented | [MUNINN_MEMORY_PROTOCOL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MUNINN_MEMORY_PROTOCOL_PROPOSAL.md), [PERSONALITY_AND_CONTEXT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PERSONALITY_AND_CONTEXT_PROPOSAL.md), and [PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md) | typed context projection path is now smoke-green for the current cognitive request path through `philote` and `model-router`; the first typed `role_activation` object now flows into session/context projection, and next pressure is expanding role addendum/toolset/skillset and hook-backed refresh |
| Tooling and execution | partially implemented | [TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md) and [MODEL_CONTROLLER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MODEL_CONTROLLER_PROPOSAL.md) | structured model envelope and initial `request_class` routing are now smoke-green for the current cognitive path; next pressure is broader structured failures, embedding support, and role-scoped toolsets |
| Operator and control plane | proposed to early transitional | [ROLE_POSTURE_AND_ADMIN_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROLE_POSTURE_AND_ADMIN_PROPOSAL.md), [CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md) | elevation, admin workflows, perimeter trust and egress |
| Deployment and distribution | implemented boundary, incomplete rollout | [RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md) | real VPS smoke, secret handling hardening, artifact distribution |
| Migration and parity | in planning | [OPENCLAW_PARITY_MIGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/OPENCLAW_PARITY_MIGRATION_PROPOSAL.md) | explicit parity matrix and migration-critical seams |

## Documentation Maintenance Rule

When a slice lands:

1. Update this file if the answer to "what is implemented" or "what is active right now" changed.
2. Update the relevant proposal disposition/current-slice text.
3. Update [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md) if sequencing or work ownership changed.

## Related Entry Points

- [docs/README.md](/Users/jaredlikes/code/philotic-stack/docs/README.md)
- [README.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/README.md)
- [ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE.md)
- [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
