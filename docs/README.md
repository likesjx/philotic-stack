# Philotic Stack — Documentation Index

> **Last Updated:** 2026-03-10

---

## Architecture Reference

Core reference documents describing the implemented system.

| Document | Description |
| -------- | ----------- |
| [ARCHITECTURE.md](architecture/ARCHITECTURE.md) | Full system design — hotel model, crates, IPC, mesh, storage, guest lifecycle |
| [PORT_BLUEPRINT.md](architecture/PORT_BLUEPRINT.md) | Migration plan from legacy plugin model |
| [CODEBASE_HEALTH.md](architecture/CODEBASE_HEALTH.md) | Static analysis and honest assessment of current codebase state (2026-03-10) |

---

## Architecture Proposals

Design proposals for features in progress or planned. These are **not** reference docs — they describe intended future behavior.

| Document | Topic |
| -------- | ----- |
| [AGENT_INCARNATION_PROPOSAL.md](architecture/AGENT_INCARNATION_PROPOSAL.md) | Conversational/worker/subagent taxonomy, session ownership |
| [AGENT_LOOP_PROPOSAL.md](architecture/AGENT_LOOP_PROPOSAL.md) | Multi-turn tool re-entry, media routing policy, approval granularity |
| [AGENT_LOOP_RESEARCH.md](architecture/AGENT_LOOP_RESEARCH.md) | Research backing the agent loop proposal |
| [AGENT_WORKFLOW_PROPOSAL.md](architecture/AGENT_WORKFLOW_PROPOSAL.md) | High-level agent workflow orchestration |
| [APPROVAL_UX_PROPOSAL.md](architecture/APPROVAL_UX_PROPOSAL.md) | Human-in-the-loop approval flow design |
| [FORKED_SESSIONS_PROPOSAL.md](architecture/FORKED_SESSIONS_PROPOSAL.md) | Session forking and delegation model |
| [GUEST_BINARY_RESOLUTION_PROPOSAL.md](architecture/GUEST_BINARY_RESOLUTION_PROPOSAL.md) | How guest binaries are located and resolved |
| [HEGEMON_COMPONENT_PROPOSAL.md](architecture/HEGEMON_COMPONENT_PROPOSAL.md) | Hegemon as outside-world membrane — transport, guard, session binding |
| [HEURISTIC_MIND_AND_CONTEXT_PAPER.md](architecture/HEURISTIC_MIND_AND_CONTEXT_PAPER.md) | Cognitive model and context structure theory |
| [INTER_HOTEL_ROUTING_PROPOSAL.md](architecture/INTER_HOTEL_ROUTING_PROPOSAL.md) | Cross-hotel event routing and execution plane design |
| [KEY_VAULT_PROPOSAL.md](architecture/KEY_VAULT_PROPOSAL.md) | Keychain-backed hotel vault root key |
| [MODEL_CONTROLLER_PROPOSAL.md](architecture/MODEL_CONTROLLER_PROPOSAL.md) | Model controller abstraction, request/response envelope design |
| [MUNINN_MEMORY_PROTOCOL_PROPOSAL.md](architecture/MUNINN_MEMORY_PROTOCOL_PROPOSAL.md) | Persistent cognitive memory protocol |
| [NATIVE_OVERLAY_VPN_PROPOSAL.md](architecture/NATIVE_OVERLAY_VPN_PROPOSAL.md) | Native overlay to replace Tailscale/WireGuard underlay |
| [PERSONALITY_AND_CONTEXT_PROPOSAL.md](architecture/PERSONALITY_AND_CONTEXT_PROPOSAL.md) | Agent identity, persona, and context projection |
| [PHILOTIC_AGENT_LOOP_PROPOSAL.md](architecture/PHILOTIC_AGENT_LOOP_PROPOSAL.md) | Philotic-specific agent loop design |
| [PHILOTIC_AGENT_LOOP_SPEC.md](architecture/PHILOTIC_AGENT_LOOP_SPEC.md) | Specification-level detail for the agent loop |
| [RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md](architecture/RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md) | VPS deployment and hotel rendering via RH Ansible |
| [RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md](architecture/RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md) | Build and distribution of guest runner artifacts |
| [RUST_FORGE_PROPOSAL.md](architecture/RUST_FORGE_PROPOSAL.md) | Rust-native tooling and build infrastructure |
| [SESSION_LOOP_PROPOSAL.md](architecture/SESSION_LOOP_PROPOSAL.md) | Session loop state machine |
| [SLASH_COMMANDS_PROPOSAL.md](architecture/SLASH_COMMANDS_PROPOSAL.md) | Slash command dispatch system |
| [TASK_RUNNER_PROPOSAL.md](architecture/TASK_RUNNER_PROPOSAL.md) | Task runner and tool assembly execution |
| [TELEGRAM_INTEGRATION_PROPOSAL.md](architecture/TELEGRAM_INTEGRATION_PROPOSAL.md) | Telegram transport integration details |
| [TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md](architecture/TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md) | Tool assembly, routing, and sandboxed execution |
| [TOOL_MANAGEMENT_PLANE_PROPOSAL.md](architecture/TOOL_MANAGEMENT_PLANE_PROPOSAL.md) | Tool catalog and management plane |
| [VOICE_MACHINE_PROPOSAL.md](architecture/VOICE_MACHINE_PROPOSAL.md) | Voice machine — audio delivery and session behavior |
| [ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md](architecture/ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md) | Migration path from legacy ZeroClaw |

---

## Operations

| Document | Description |
| -------- | ----------- |
| [worktree-workflow.md](worktree-workflow.md) | Parallel workstream and worktree workflow guide |
| [task.md](task.md) | Current task tracking |

---

## Quick Reference

- **IPC socket:** `/tmp/philotic-ansible.sock` (env: `PHILOTIC_HOTEL_SOCKET`)
- **Mesh UDP port:** `8999`
- **Blob HTTP port:** `9001`
- **Execution TCP port:** `mesh_port + 2`
- **Context DB:** `ansible_context.db` (SQLite)
- **Guest supervisor interval:** 5 seconds
