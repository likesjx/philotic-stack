---
title: "Philotic Architecture Status"
doc_type: status
domain: runtime-sessions
status: active
last_updated: 2026-03-12
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

> **Status:** Living Snapshot | **Last Updated:** 2026-03-12

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

## Current Architecture Summary

Philotic currently operates as a hotel-centered runtime:

- `ansible` is the runtime authority for hotel orchestration, guest materialization, context-graph persistence, IPC handling, and inter-hotel coordination.
- `membrane`, `agent-core`, `model-router`, and `tool-runner` are separate guest-facing binaries with explicit runtime boundaries.
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
- role incarnation design direction is adopted, and the first graph/routing substrate now exists, but the full role/toolset/handoff materialization path is not yet implemented

## Active Work Right Now

These are the most clearly active seams as of 2026-03-12:

| Seam | Current truth | Next pressure |
| --- | --- | --- |
| Session leases and ownership semantics | session durability, approval state, and timeline projection exist; explicit active-work ownership semantics are still incomplete in the task board | define and implement canonical active ownership without creating a second authority shadow |
| Runtime authority leases | a shared `LeaseEnvelope` and central runtime lease registry/provider now exist, Telegram poll lease has been migrated onto that abstraction, and the first explicit boundary contract now separates lease authority from materialization, supervision, routing, and vault access | move the next runtime seam onto the shared provider path and prove the contract on a non-Telegram path |
| Telegram membrane authority | poll-lease acquire, renew, expiry, home-hotel checks, graceful release, dual-poller smoke coverage, and explicit delegated remote polling are implemented | canonical mesh-visible poll authority is still deferred |
| Mesh-visible state placement | current local authorities mostly live in hotel runtime state, SQLite, or file-backed records; shared criteria for what becomes mesh-visible are now being defined explicitly | classify current state families and stop solving each cross-hotel visibility seam with a bespoke projection ritual |
| Role incarnation model | `RoleIncarnationRecord`, `TurnLoopConfig`, session `active_incarnation_id`, inbound agent-task routing to the active incarnation, and orchestrator fallback for missing active guests now exist; handoff/materialization behavior remains incomplete | implement role provisioning, buffered routing/materialization, handoff IPC, and role-profile materialization |
| Tool execution envelope | catalog-backed tools and approval policy exist | extend structured error behavior across more routed components instead of falling back to ad hoc strings |
| Perimeter egress control | proposal exists and the lack of a unified egress plane is explicitly called out | define the first policy object and classify current egress exceptions |
| Deployment hardening | VPS boundary and peer rendering contract are defined | remove plaintext secret assumptions and prove real VPS smokes |

## Domain Status Matrix

| Domain | Status | Source of truth | Active work |
| --- | --- | --- | --- |
| Runtime and sessions | implemented, still evolving | [SESSION_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SESSION_LOOP_PROPOSAL.md) and code in `ansible`, `agent-core`, `ansible-mesh-core` | session ownership semantics, compaction policy, bounded loop follow-through |
| Membrane and transport | implemented, still evolving | [TELEGRAM_INTEGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_INTEGRATION_PROPOSAL.md) and [TELEGRAM_POLL_LEASE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_POLL_LEASE_PROPOSAL.md) | delegated poll authority, broader transport surfaces |
| Mesh and placement | partially implemented | [INTER_HOTEL_ROUTING_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/INTER_HOTEL_ROUTING_PROPOSAL.md), [MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md) | placement policy, trust boundaries, overlay evolution, and mesh-visible state classification |
| Memory and context | partially implemented | [MUNINN_MEMORY_PROTOCOL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MUNINN_MEMORY_PROTOCOL_PROPOSAL.md) and [PERSONALITY_AND_CONTEXT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PERSONALITY_AND_CONTEXT_PROPOSAL.md) | pluggable context/memory engines |
| Tooling and execution | partially implemented | [TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md) | structured failures, tool management plane, role-scoped toolsets |
| Operator and control plane | proposed to early transitional | [ROLE_POSTURE_AND_ADMIN_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROLE_POSTURE_AND_ADMIN_PROPOSAL.md), [CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md) | elevation, admin workflows, perimeter trust and egress |
| Deployment and distribution | implemented boundary, incomplete rollout | [RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md) | real VPS smoke, secret handling hardening, artifact distribution |
| Migration and parity | in planning | [OPENCLAW_PARITY_MIGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/OPENCLAW_PARITY_MIGRATION_PROPOSAL.md) | explicit parity matrix and migration-critical seams |

## Documentation Maintenance Rule

When a slice lands:

1. Update this file if the answer to "what is implemented" or "what is active right now" changed.
2. Update the relevant proposal disposition/current-slice text.
3. Update [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md) if sequencing or work ownership changed.

## Related Entry Points

- [README.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/README.md)
- [ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE.md)
- [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
