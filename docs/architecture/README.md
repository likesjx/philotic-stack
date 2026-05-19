---
title: "Philotic Architecture Map"
doc_type: workflow
domain: workflow-docs
status: active
last_updated: 2026-04-10
tags:
  - docs
  - architecture
  - domains
  - source-of-truth
related_docs:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
  - DOMAIN_MAP.md
  - SEAM_REGISTRY.md
  - DOC_TAGGING_FRONTMATTER_PROPOSAL.md
task_refs:
  - docs/task.md
---

# Philotic Architecture Map

> **Status:** Living Index | **Last Updated:** 2026-04-10

This directory now has one explicit split:

- [ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md) is the current source of truth for what is implemented, what is actively being worked, and which seams are still transitional.
- [ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE.md) is the deeper architecture reference for runtime shape, crate boundaries, and core protocols.
- [DOMAIN_MAP.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DOMAIN_MAP.md) is the scope-first navigation map for domains, active proposals, and adjacent docs.
- [SEAM_REGISTRY.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SEAM_REGISTRY.md) is the stable ID registry for active seams.
- proposal docs in this directory describe intended or evolving design, not automatically implemented truth.

If those ever disagree, observed code and tests win, then `ARCHITECTURE_STATUS.md`, then older narrative docs. Software does love pretending every markdown file is equally current.

That precedence also applies when a crate README, quickstart note, or older architecture narrative disagrees on concrete runtime details like socket paths, port ownership, or whether a boundary is still transitional.

## Domain Map

Use these domains as the lightweight organization layer for architecture work. They are retrieval aids and ownership hints, not a folder explosion.

For the full scope-first catalog, see [DOMAIN_MAP.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DOMAIN_MAP.md).

For stable active seam IDs, see [SEAM_REGISTRY.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SEAM_REGISTRY.md).

| Domain | What belongs here | Start here |
| --- | --- | --- |
| Runtime and Sessions | hotel authority, session model, approvals, routing, working-turn behavior, runtime authority leases, governed workflow skills, scripted turn loop variants | [ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md), [SESSION_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SESSION_LOOP_PROPOSAL.md), [AGENT_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_LOOP_PROPOSAL.md), [RUNTIME_AUTHORITY_LEASES_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RUNTIME_AUTHORITY_LEASES_PROPOSAL.md), [GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md), [ROLE_CONTEXT_SHIFT_AND_DELEGATED_SUBAGENTS_WHITEPAPER.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROLE_CONTEXT_SHIFT_AND_DELEGATED_SUBAGENTS_WHITEPAPER.md), [ROLE_ACTIVATION_AND_SUBAGENT_CONTRACTS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROLE_ACTIVATION_AND_SUBAGENT_CONTRACTS_PROPOSAL.md), [SCRIPTED_TURN_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SCRIPTED_TURN_LOOP_PROPOSAL.md) |
| Membrane and Transport | Telegram ingress/egress, poll leases, slash commands, external channel boundaries, desktop/operator membranes, external agent/event membranes, and mesh transport boundaries | [TELEGRAM_INTEGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_INTEGRATION_PROPOSAL.md), [TELEGRAM_POLL_LEASE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_POLL_LEASE_PROPOSAL.md), [MEMBRANE_COMPONENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMBRANE_COMPONENT_PROPOSAL.md), [DESKTOP_MEMBRANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DESKTOP_MEMBRANE_PROPOSAL.md), [OPERATOR_MEMBRANE_PLUGIN_BOUNDARY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/OPERATOR_MEMBRANE_PLUGIN_BOUNDARY_PROPOSAL.md), [MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md), [MESH_SYNC_AND_TRANSPORT_BOUNDARIES_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MESH_SYNC_AND_TRANSPORT_BOUNDARIES_PROPOSAL.md) |
| Operator Control Plane | desktop/system settings boundaries, hotel-owned operator auth, auth bootstrap strategy, remote admin surfaces, workspace app publication, philote-facing desktop customization | [CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md), [HOTEL_USER_IDENTITY_AND_OPERATOR_AUTH_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HOTEL_USER_IDENTITY_AND_OPERATOR_AUTH_PROPOSAL.md), [OPERATOR_AUTH_BOOTSTRAP_STRATEGY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/OPERATOR_AUTH_BOOTSTRAP_STRATEGY_PROPOSAL.md), [DESKTOP_WORKSPACE_COMPONENTS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DESKTOP_WORKSPACE_COMPONENTS_PROPOSAL.md), [REMOTE_HOTEL_ADMIN_PARITY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/REMOTE_HOTEL_ADMIN_PARITY_PROPOSAL.md) |
| Mesh and Placement | inter-hotel routing, execution transport, placement, overlay reachability, mesh-visible state | [INTER_HOTEL_ROUTING_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/INTER_HOTEL_ROUTING_PROPOSAL.md), [NATIVE_OVERLAY_VPN_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/NATIVE_OVERLAY_VPN_PROPOSAL.md), [MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md), [MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md) |
| Memory and Context | Muninn protocol, context assembly, personality projection, memory engines, relational memory lifecycle, memory layering vs. work-product truth | [MUNINN_MEMORY_PROTOCOL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MUNINN_MEMORY_PROTOCOL_PROPOSAL.md), [PERSONALITY_AND_CONTEXT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PERSONALITY_AND_CONTEXT_PROPOSAL.md), [MEMORY_LAYERING_AND_WORK_PRODUCT_SPLIT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMORY_LAYERING_AND_WORK_PRODUCT_SPLIT_PROPOSAL.md), [PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md), [MEMORY_RELATION_LIFECYCLE_WHITEPAPER.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMORY_RELATION_LIFECYCLE_WHITEPAPER.md) |
| Tooling and Execution | tool assembly, tool management plane, task runners, plugin seams, model-controller routing | [TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md), [TOOL_MANAGEMENT_PLANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_MANAGEMENT_PLANE_PROPOSAL.md), [TASK_RUNNER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TASK_RUNNER_PROPOSAL.md), [COMPUTER_USE_TASK_RUNNER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/COMPUTER_USE_TASK_RUNNER_PROPOSAL.md), [MODEL_CONTROLLER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MODEL_CONTROLLER_PROPOSAL.md), [MODEL_GRAPH_AND_CONTEXT_1_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MODEL_GRAPH_AND_CONTEXT_1_PROPOSAL.md) |
| Operator and Control Plane | admin posture, operator identity, auth bootstrap strategy, approval UX, trust, egress, observability, routed operator chat, desktop component authoring, remote hotel admin parity | [ROLE_POSTURE_AND_ADMIN_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROLE_POSTURE_AND_ADMIN_PROPOSAL.md), [CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md), [HOTEL_USER_IDENTITY_AND_OPERATOR_AUTH_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HOTEL_USER_IDENTITY_AND_OPERATOR_AUTH_PROPOSAL.md), [OPERATOR_AUTH_BOOTSTRAP_STRATEGY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/OPERATOR_AUTH_BOOTSTRAP_STRATEGY_PROPOSAL.md), [ROUTED_OPERATOR_CHAT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROUTED_OPERATOR_CHAT_PROPOSAL.md), [PERIMETER_EGRESS_CONTROL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PERIMETER_EGRESS_CONTROL_PROPOSAL.md), [DESKTOP_COMPONENT_AUTHORING_PARITY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DESKTOP_COMPONENT_AUTHORING_PARITY_PROPOSAL.md), [REMOTE_HOTEL_ADMIN_PARITY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/REMOTE_HOTEL_ADMIN_PARITY_PROPOSAL.md), [COMPONENT_TEMPLATE_SCHEMA_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/COMPONENT_TEMPLATE_SCHEMA_PROPOSAL.md) |
| Operator and Control Plane | admin posture, approval UX, trust, egress, observability, routed operator chat, desktop component authoring, remote hotel admin parity, operator identity and dangerous-action ceremony | [ROLE_POSTURE_AND_ADMIN_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROLE_POSTURE_AND_ADMIN_PROPOSAL.md), [CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md), [ROUTED_OPERATOR_CHAT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROUTED_OPERATOR_CHAT_PROPOSAL.md), [PERIMETER_EGRESS_CONTROL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PERIMETER_EGRESS_CONTROL_PROPOSAL.md), [DESKTOP_COMPONENT_AUTHORING_PARITY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DESKTOP_COMPONENT_AUTHORING_PARITY_PROPOSAL.md), [REMOTE_HOTEL_ADMIN_PARITY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/REMOTE_HOTEL_ADMIN_PARITY_PROPOSAL.md), [OPERATOR_IDENTITY_AND_DANGEROUS_ACTION_CEREMONIES_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/OPERATOR_IDENTITY_AND_DANGEROUS_ACTION_CEREMONIES_PROPOSAL.md), [COMPONENT_TEMPLATE_SCHEMA_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/COMPONENT_TEMPLATE_SCHEMA_PROPOSAL.md) |
| Deployment and Distribution | VPS deployment, binary resolution, build/distribution contracts, Homebrew | [RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md), [GUEST_BINARY_RESOLUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/GUEST_BINARY_RESOLUTION_PROPOSAL.md), [RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md) |
| Migration and Historical Direction | parity work, legacy bridge, historical blueprints, research notes | [OPENCLAW_PARITY_MIGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/OPENCLAW_PARITY_MIGRATION_PROPOSAL.md), [ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md), [PORT_BLUEPRINT.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PORT_BLUEPRINT.md) |

## Source-of-Truth Rules

When updating docs, keep these boundaries explicit:

1. Observed code and tests win over prose when they disagree.
2. Update [ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md) when implemented behavior, active seams, or the repo's honest current state changes.
3. Update [ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE.md) when the durable architecture reference itself changes.
4. Update a proposal when the recommendation, disposition, or current slice changes.
5. Update [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md) when the work surface or sequencing changes.

`docs/task.md` is the execution surface, not the runtime-protocol reference. Use it to see what is being worked, not to settle socket, transport, or ownership disputes.

## Metadata Strategy

Active architecture and workflow docs should move toward lightweight YAML frontmatter with:

- one primary `domain`
- one `doc_type`
- explicit `status`
- small retrieval-oriented `tags`
- link fields for related docs and task references

See [DOC_TAGGING_FRONTMATTER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DOC_TAGGING_FRONTMATTER_PROPOSAL.md) for the current metadata strategy and controlled vocabulary.

## Seam Doc Rule

Seam docs are optional.

Default rule:

- keep seams in proposal docs, [SEAM_REGISTRY.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SEAM_REGISTRY.md), and [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
- only create a seam doc when the seam becomes cross-cutting, repeatedly confusing, verification-heavy, or duplicated enough to justify its own boundary narrative

Do not graduate a seam into its own file just because it has become long. Graduate it when people keep paying the confusion tax.

## Session Refresh

Use `SVE refresh` as the standard shorthand for reloading the current Philotic SVE process in an open session.

That refresh should re-read:

- [AGENTS.md](/Users/jaredlikes/code/philotic-stack/AGENTS.md)
- [README.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/README.md)
- [ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md)
- [DOC_TAGGING_FRONTMATTER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DOC_TAGGING_FRONTMATTER_PROPOSAL.md)

## Proposal Hygiene

Architecture proposals should prefer this minimum shape:

- `Goal`
- `Core Recommendation`
- `Disposition`
- `Current Slice`
- task links

Recommended disposition vocabulary:

- `proposed`
- `accepted for current slice`
- `implemented`
- `superseded`
- `deferred`

## Current Gaps Worth Cleaning Later

- [ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE.md) still carries some older wording around mesh/data-plane details and should be tightened against the current execution-transport reality.
- some crate READMEs still carry older `Ansible` naming, port-first launch examples, or stale IPC wording. Treat them as convenience docs unless they match current code and [docs/README.md](/Users/jaredlikes/code/philotic-stack/docs/README.md).
- [docs/PHILOTIC-ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/PHILOTIC-ARCHITECTURE.md) is historical and should not be treated as current architecture truth.
- [PORT_BLUEPRINT.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PORT_BLUEPRINT.md), [docs/ARCHITECT_THOUGHTS_CONTEXT_GRAPH.md](/Users/jaredlikes/code/philotic-stack/docs/ARCHITECT_THOUGHTS_CONTEXT_GRAPH.md), and [docs/walkthrough.md](/Users/jaredlikes/code/philotic-stack/docs/walkthrough.md) are now explicitly historical and should be used for lineage, not current authority.
