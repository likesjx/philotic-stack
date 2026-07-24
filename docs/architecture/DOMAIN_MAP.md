---
title: Philotic Architecture Domain Map
doc_type: workflow
domain: workflow-docs
status: active
last_updated: 2026-07-24
tags:
- docs
- domains
- architecture
- navigation
related_docs:
- README.md
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
task_refs:
- docs/task.md
---

# Philotic Architecture Domain Map

This is the scope-first navigation view for `docs/architecture/`.

It is also the authoritative catalog of domain nodes for the graph. When a
new domain is needed, inspect the existing catalog first, then add a new
section here and let the graph scanner materialize the domain node from this
document.

Use it when you know the concern area first and need to find:

- current truth
- active proposals
- adjacent docs in the same scope

If you want the current system snapshot first, start at [ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md).

## Runtime And Sessions

Primary domain id: `runtime-sessions`

Current truth:

- [ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md)
- [ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE.md)

Active proposals:

- [SESSION_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SESSION_LOOP_PROPOSAL.md)
- [AGENT_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_LOOP_PROPOSAL.md)
- [AGENT_INCARNATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_INCARNATION_PROPOSAL.md)
- [APPROVAL_UX_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/APPROVAL_UX_PROPOSAL.md)
- [FORKED_SESSIONS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/FORKED_SESSIONS_PROPOSAL.md)

Supporting docs:

- [PHILOTIC_AGENT_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PHILOTIC_AGENT_LOOP_PROPOSAL.md)
- [PHILOTIC_AGENT_LOOP_SPEC.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PHILOTIC_AGENT_LOOP_SPEC.md)
- [AGENT_CONTEXT_MANAGEMENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_CONTEXT_MANAGEMENT_PROPOSAL.md)

## Membrane And Transport

Primary domain id: `membrane-transport`

Current truth:

- [ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md)
- [ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE.md)

Active proposals:

- [TELEGRAM_INTEGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_INTEGRATION_PROPOSAL.md)
- [TELEGRAM_POLL_LEASE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_POLL_LEASE_PROPOSAL.md)
- MEMBRANE_TRANSPORT_HOME_PROPOSAL (in intel-graph)
- [MEMBRANE_COMPONENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMBRANE_COMPONENT_PROPOSAL.md)
- [DISCORD_MEMBRANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DISCORD_MEMBRANE_PROPOSAL.md)
- [DESKTOP_MEMBRANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DESKTOP_MEMBRANE_PROPOSAL.md)
- [MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md)
- [SLASH_COMMANDS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SLASH_COMMANDS_PROPOSAL.md)
- [VOICE_MACHINE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/VOICE_MACHINE_PROPOSAL.md)

## Mesh And Placement

Primary domain id: `mesh-placement`

Current truth:

- [ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md)
- [ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE.md)

Active proposals:

- [INTER_HOTEL_ROUTING_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/INTER_HOTEL_ROUTING_PROPOSAL.md)
- [MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md)
- [NATIVE_OVERLAY_VPN_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/NATIVE_OVERLAY_VPN_PROPOSAL.md)
- [HOTEL_PERIMETER_TRUST_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HOTEL_PERIMETER_TRUST_PROPOSAL.md)
- [ROUTER_NATIVE_OBSERVABILITY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROUTER_NATIVE_OBSERVABILITY_PROPOSAL.md)

## Memory And Context

Primary domain id: `memory-context`

Current truth:

- [ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md)

Active proposals:

- [KNOWLEDGE_ARCHITECTURE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/KNOWLEDGE_ARCHITECTURE_PROPOSAL.md)
- [MEMPALACE_EPISODIC_MEMORY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMPALACE_EPISODIC_MEMORY_PROPOSAL.md)
- [OBSIDIAN_KNOWLEDGE_GARDEN_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/OBSIDIAN_KNOWLEDGE_GARDEN_PROPOSAL.md)
- [CREATIVE_LEARNING_FLYWHEEL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/CREATIVE_LEARNING_FLYWHEEL_PROPOSAL.md)
- [MUNINN_MEMORY_PROTOCOL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MUNINN_MEMORY_PROTOCOL_PROPOSAL.md)
- [MUNINN_V07_CAPABILITY_ADOPTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MUNINN_V07_CAPABILITY_ADOPTION_PROPOSAL.md)
- [MEMORY_CULTIVATION_TRUE_UP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMORY_CULTIVATION_TRUE_UP_PROPOSAL.md)
- [PERSONALITY_AND_CONTEXT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PERSONALITY_AND_CONTEXT_PROPOSAL.md)
- [PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md)
- [MEMORY_ENGINE_ABSTRACTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMORY_ENGINE_ABSTRACTION_PROPOSAL.md)

Supporting docs:

- [LIFE_GRAPH_OS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/LIFE_GRAPH_OS_PROPOSAL.md)
- [HEURISTIC_MIND_AND_CONTEXT_PAPER.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HEURISTIC_MIND_AND_CONTEXT_PAPER.md)

## Tooling And Execution

Primary domain id: `tooling-execution`

Current truth:

- [ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md)

Active proposals:

- [TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md)
- [TOOL_MANAGEMENT_PLANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_MANAGEMENT_PLANE_PROPOSAL.md)
- [TASK_RUNNER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TASK_RUNNER_PROPOSAL.md)
- [COMPUTER_USE_TASK_RUNNER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/COMPUTER_USE_TASK_RUNNER_PROPOSAL.md)
- [MODEL_CONTROLLER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MODEL_CONTROLLER_PROPOSAL.md)
- [AGENT_PLUGIN_HOOKS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_PLUGIN_HOOKS_PROPOSAL.md)

## Operator And Control Plane

Primary domain id: `operator-control-plane`

Current truth:

- [ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md)

Active proposals:

- [ROLE_POSTURE_AND_ADMIN_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROLE_POSTURE_AND_ADMIN_PROPOSAL.md)
- [CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md)
- [PERIMETER_EGRESS_CONTROL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PERIMETER_EGRESS_CONTROL_PROPOSAL.md)
- [KEY_VAULT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/KEY_VAULT_PROPOSAL.md)
- [LOCAL_ADMIN_FALLBACK_MODEL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/LOCAL_ADMIN_FALLBACK_MODEL_PROPOSAL.md)

## Deployment And Distribution

Primary domain id: `deployment-distribution`

Current truth:

- [ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md)
- [ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE.md)

Active proposals:

- [RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md)
- [GUEST_BINARY_RESOLUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/GUEST_BINARY_RESOLUTION_PROPOSAL.md)
- [RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md)
- [HOMEBREW_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HOMEBREW_DISTRIBUTION_PROPOSAL.md)
- [RUST_FORGE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RUST_FORGE_PROPOSAL.md)

## Product Management Plane

Primary domain id: `product-management-plane`

Current truth:

- [GRAPH_AS_SOURCE_OF_TRUTH.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/GRAPH_AS_SOURCE_OF_TRUTH.md)
- [ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md)
- [ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE.md)

Active proposals:

- [GRAPH_INTELLIGENCE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/GRAPH_INTELLIGENCE_PROPOSAL.md)
- [PHILOTIC_WEB_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PHILOTIC_WEB_PROPOSAL.md)

Supporting docs:

- [DOC_TAGGING_FRONTMATTER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DOC_TAGGING_FRONTMATTER_PROPOSAL.md)
- [VERIFICATION_LADDER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/VERIFICATION_LADDER_PROPOSAL.md)

## Migration And Parity

Primary domain id: `migration-parity`

Current truth:

- [ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md)

Active proposals:

- [OPENCLAW_PARITY_MIGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/OPENCLAW_PARITY_MIGRATION_PROPOSAL.md)
- [ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md)

Supporting docs:

- [AGENT_LOOP_RESEARCH.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_LOOP_RESEARCH.md)

Historical but still relevant:

- [PORT_BLUEPRINT.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PORT_BLUEPRINT.md)
- [PHILOTIC-ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/PHILOTIC-ARCHITECTURE.md)
- [ARCHITECT_THOUGHTS_CONTEXT_GRAPH.md](/Users/jaredlikes/code/philotic-stack/docs/ARCHITECT_THOUGHTS_CONTEXT_GRAPH.md)

## Workflow Docs

Primary domain id: `workflow-docs`

Current truth:

- [README.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/README.md)
- [DOC_TAGGING_FRONTMATTER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DOC_TAGGING_FRONTMATTER_PROPOSAL.md)

Active proposals:

- [AGENT_WORKFLOW_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_WORKFLOW_PROPOSAL.md)
- [PROPOSAL_ORGANIZATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PROPOSAL_ORGANIZATION_PROPOSAL.md)
- [DEV_ENGINE_OPTIMIZATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DEV_ENGINE_OPTIMIZATION_PROPOSAL.md)

Supporting docs:

- [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
