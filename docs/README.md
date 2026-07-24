# Philotic Stack — Documentation Index

> **Last Updated:** 2026-04-24

---

## Start Here

Use these in order when you want the current architecture without archaeology:

| Document | Description |
| -------- | ----------- |
| [architecture/README.md](architecture/README.md) | Architecture hub, domain map, and doc update rules |
| [architecture/ARCHITECTURE_STATUS.md](architecture/ARCHITECTURE_STATUS.md) | Single source of truth for what is implemented, transitional, and actively in flight |
| [architecture/DOMAIN_MAP.md](architecture/DOMAIN_MAP.md) | Scope-first catalog of domains, active proposals, and adjacent docs |
| [architecture/SEAM_REGISTRY.md](architecture/SEAM_REGISTRY.md) | Stable IDs for active seams across proposals and task tracking |
| [architecture/ARCHITECTURE.md](architecture/ARCHITECTURE.md) | Deep architecture reference for runtime structure and major protocols |
| [task.md](task.md) | Current work surface and sequencing |

Source-of-truth order for architecture questions:

1. observed code and tests
2. [architecture/ARCHITECTURE_STATUS.md](architecture/ARCHITECTURE_STATUS.md)
3. [architecture/ARCHITECTURE.md](architecture/ARCHITECTURE.md)
4. proposal and historical docs

Use [task.md](task.md) for execution sequencing, not to settle runtime protocol details when another doc or the code disagrees.

## Architecture Reference

Core documents describing implemented or current-system truth.

| Document | Description |
| -------- | ----------- |
| [architecture/ARCHITECTURE_STATUS.md](architecture/ARCHITECTURE_STATUS.md) | Current architecture snapshot: implemented behavior, transitional seams, active work |
| [architecture/ARCHITECTURE.md](architecture/ARCHITECTURE.md) | Deep runtime design reference — hotel model, crates, IPC, mesh, storage, guest lifecycle |
| [architecture/CODEBASE_HEALTH.md](architecture/CODEBASE_HEALTH.md) | Honest assessment of current codebase state |
| [architecture/CONCURRENCY_STRATEGY.md](architecture/CONCURRENCY_STRATEGY.md) | Concurrency audit and prioritized strategy |

---

## Architecture Domains

Domain-level organization for proposals and active design work lives in [architecture/README.md](architecture/README.md).

These docs are proposal space, not automatic runtime truth:

| Domain | Examples |
| ----- | -------- |
| Runtime and Sessions | [architecture/SESSION_LOOP_PROPOSAL.md](architecture/SESSION_LOOP_PROPOSAL.md), [architecture/AGENT_LOOP_PROPOSAL.md](architecture/AGENT_LOOP_PROPOSAL.md), [architecture/AGENT_INCARNATION_PROPOSAL.md](architecture/AGENT_INCARNATION_PROPOSAL.md), [architecture/GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md](architecture/GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md) |
| Membrane and Transport | [architecture/TELEGRAM_INTEGRATION_PROPOSAL.md](architecture/TELEGRAM_INTEGRATION_PROPOSAL.md), [architecture/TELEGRAM_POLL_LEASE_PROPOSAL.md](architecture/TELEGRAM_POLL_LEASE_PROPOSAL.md), [architecture/SLASH_COMMANDS_PROPOSAL.md](architecture/SLASH_COMMANDS_PROPOSAL.md), [architecture/DESKTOP_MEMBRANE_PROPOSAL.md](architecture/DESKTOP_MEMBRANE_PROPOSAL.md) |
| Mesh and Placement | [architecture/INTER_HOTEL_ROUTING_PROPOSAL.md](architecture/INTER_HOTEL_ROUTING_PROPOSAL.md), [architecture/NATIVE_OVERLAY_VPN_PROPOSAL.md](architecture/NATIVE_OVERLAY_VPN_PROPOSAL.md), [architecture/MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md](architecture/MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md) |
| Memory and Context | [architecture/MUNINN_MEMORY_PROTOCOL_PROPOSAL.md](architecture/MUNINN_MEMORY_PROTOCOL_PROPOSAL.md), [architecture/PERSONALITY_AND_CONTEXT_PROPOSAL.md](architecture/PERSONALITY_AND_CONTEXT_PROPOSAL.md), [architecture/PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md](architecture/PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md), [architecture/LIFE_GRAPH_OS_PROPOSAL.md](architecture/LIFE_GRAPH_OS_PROPOSAL.md), [architecture/MEMORY_CULTIVATION_TRUE_UP_PROPOSAL.md](architecture/MEMORY_CULTIVATION_TRUE_UP_PROPOSAL.md) |
| Tooling and Execution | [architecture/TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md](architecture/TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md), [architecture/TASK_RUNNER_PROPOSAL.md](architecture/TASK_RUNNER_PROPOSAL.md), [architecture/TOOL_MANAGEMENT_PLANE_PROPOSAL.md](architecture/TOOL_MANAGEMENT_PLANE_PROPOSAL.md), [architecture/COMPUTER_USE_TASK_RUNNER_PROPOSAL.md](architecture/COMPUTER_USE_TASK_RUNNER_PROPOSAL.md) |
| Operator and Control Plane | [architecture/ROLE_POSTURE_AND_ADMIN_PROPOSAL.md](architecture/ROLE_POSTURE_AND_ADMIN_PROPOSAL.md), [architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md](architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md), [architecture/PERIMETER_EGRESS_CONTROL_PROPOSAL.md](architecture/PERIMETER_EGRESS_CONTROL_PROPOSAL.md) |
| Deployment and Distribution | [architecture/RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md](architecture/RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md), [architecture/GUEST_BINARY_RESOLUTION_PROPOSAL.md](architecture/GUEST_BINARY_RESOLUTION_PROPOSAL.md), [architecture/HOMEBREW_DISTRIBUTION_PROPOSAL.md](architecture/HOMEBREW_DISTRIBUTION_PROPOSAL.md) |
| Migration and Historical Direction | [architecture/OPENCLAW_PARITY_MIGRATION_PROPOSAL.md](architecture/OPENCLAW_PARITY_MIGRATION_PROPOSAL.md), [architecture/ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md](architecture/ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md), [architecture/PORT_BLUEPRINT.md](architecture/PORT_BLUEPRINT.md) |

---

## Operations

| Document | Description |
| -------- | ----------- |
| [process/WORKFLOW.md](process/WORKFLOW.md) | Process/workflow home: SVE operating loop, rule placement, rollout truth, validation/close-out discipline |
| [worktree-workflow.md](worktree-workflow.md) | Parallel workstream and worktree workflow guide |
| [task.md](task.md) | Current task tracking |

## Historical Docs

| Document | Description |
| -------- | ----------- |
| [PHILOTIC-ARCHITECTURE.md](PHILOTIC-ARCHITECTURE.md) | Historical concept doc from earlier ZeroClaw/Philotic framing; do not treat as the current architecture source of truth |
| [ARCHITECT_THOUGHTS_CONTEXT_GRAPH.md](ARCHITECT_THOUGHTS_CONTEXT_GRAPH.md) | Historical architect-thesis narrative from earlier ZeroClaw framing; useful for lineage, not current law |
| [architecture/PORT_BLUEPRINT.md](architecture/PORT_BLUEPRINT.md) | Historical migration blueprint from an earlier port-planning phase |
| [walkthrough.md](walkthrough.md) | Historical walkthrough of an earlier end-to-end materialization path |

---

## Quick Reference

- **IPC socket:** `PHILOTIC_HOTEL_SOCKET` points to the active hotel socket; generic local default is `/tmp/philotic-aiua.sock`, while named hotels commonly materialize `/tmp/philotic-<hotel>.sock`
- **Mesh UDP port:** `8999`
- **Blob HTTP port:** `9001`
- **Execution TCP port:** `mesh_port + 2`
- **Context DB:** `aiua_context.db` (SQLite)
- **Guest supervisor interval:** 5 seconds

## Known Drift To Treat Carefully

- Some crate READMEs still use older `Ansible` naming or port-first IPC wording.
- The `ansible-mesh-core` monolith is actively being extracted into domain-specific `philotic-primitives-*` crates (mesh, hotel, agent, data, model, tool).
- [PHILOTIC-ARCHITECTURE.md](PHILOTIC-ARCHITECTURE.md), [ARCHITECT_THOUGHTS_CONTEXT_GRAPH.md](ARCHITECT_THOUGHTS_CONTEXT_GRAPH.md), [walkthrough.md](walkthrough.md), and [architecture/PORT_BLUEPRINT.md](architecture/PORT_BLUEPRINT.md) are historical context, not current authority.
