---
title: Philotic Seam Registry
doc_type: workflow
domain: workflow-docs
status: active
last_updated: 2026-06-04
tags:
- seams
- ids
- docs
- planning
related_docs:
- README.md
- DOMAIN_MAP.md
- ARCHITECTURE_STATUS.md
- DOC_TAGGING_FRONTMATTER_PROPOSAL.md
- GRAPH_AS_SOURCE_OF_TRUTH.md
- GLOSSARY.md
task_refs:
- docs/task.md
---

# Philotic Seam Registry

This document defines the stable seam IDs used to link proposals, current architecture status, and the task surface.

## Why This Exists

Proposal docs already carry `active_seams`, but until now those seam names were only stable by social agreement and good intentions.

This registry makes the seam layer explicit without forcing every task bullet to become its own permanent artifact on day one.

## Seam ID Rules

- seam IDs are stable kebab-case slugs
- a seam belongs to exactly one primary domain
- a seam may be linked from multiple proposals, but should have one primary parent proposal
- `docs/task.md` remains the execution surface; the seam registry is the identity surface
- do not recycle a seam ID for a different concern later

## Active Seams

| Seam ID | Domain | Primary proposal | Verification | Current task surface |
| --- | --- | --- | --- | --- |
| `session-leases` | `runtime-sessions` | [SESSION_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SESSION_LOOP_PROPOSAL.md) | proposed | `docs/task.md` → `WI 1: Session Management` |
| `runtime-authority-leases` | `runtime-sessions` | [RUNTIME_AUTHORITY_LEASES_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RUNTIME_AUTHORITY_LEASES_PROPOSAL.md) | uat-green | `docs/task.md` → `New Project: Runtime Authority Leases` |
| `session-compaction` | `runtime-sessions` | [SESSION_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SESSION_LOOP_PROPOSAL.md) | proposed | `docs/task.md` → `WI 2: Agent Logic` |
| `structured-context-layers` | `memory-context` | [PERSONALITY_AND_CONTEXT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PERSONALITY_AND_CONTEXT_PROPOSAL.md) | `docs/task.md` → `Next Project: Personality and Context` |
| `legacy-workspace-import` | `memory-context` | [PERSONALITY_AND_CONTEXT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PERSONALITY_AND_CONTEXT_PROPOSAL.md) | `docs/task.md` → `Next Project: Personality and Context` |
| `role-incarnation-records` | `runtime-sessions` | [AGENT_INCARNATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_INCARNATION_PROPOSAL.md) | `docs/task.md` → `New Project: Agent Incarnation Model / Role Incarnation Records` |
| `active-membrane-routing` | `runtime-sessions` | [AGENT_INCARNATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_INCARNATION_PROPOSAL.md) | `docs/task.md` → `New Project: Agent Incarnation Model / Active Membrane Routing` |
| `handoff-skill` | `runtime-sessions` | [AGENT_INCARNATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_INCARNATION_PROPOSAL.md) | `docs/task.md` → `New Project: Agent Incarnation Model / Handoff Skill + Membrane Switching` |
| `governed-workflow-skills` | `runtime-sessions` | [GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md) | `docs/task.md` → `Governed workflow skills` |
| `peer-delegation-workflows` | `runtime-sessions` | [GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md) | `docs/task.md` → `Governed workflow skills` |
| `approval-card-ux` | `runtime-sessions` | [APPROVAL_UX_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/APPROVAL_UX_PROPOSAL.md) | `docs/task.md` → `Telegram approval card UX` |
| `session-preapproval-ux` | `runtime-sessions` | [APPROVAL_UX_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/APPROVAL_UX_PROPOSAL.md) | `docs/task.md` → `Approval UX evolution` |
| `telegram-poll-lease` | `membrane-transport` | [TELEGRAM_POLL_LEASE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_POLL_LEASE_PROPOSAL.md) | smoke-green | `docs/task.md` → Telegram poll ownership slices |
| `delegated-telegram-polling` | `membrane-transport` | [TELEGRAM_POLL_LEASE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_POLL_LEASE_PROPOSAL.md) | `docs/task.md` → `Telegram poll lease mesh authority` |
| `membrane-transport-home` | `membrane-transport` | MEMBRANE_TRANSPORT_HOME_PROPOSAL (intel-graph) | proposed | `docs/task.md` → `New Project: Multi-Hotel Component Distribution` |
| `mesh-visible-poll-authority` | `membrane-transport` | MEMBRANE_TRANSPORT_HOME_PROPOSAL (intel-graph) | proposed | `docs/task.md` → `Telegram poll lease mesh authority` and transport-home follow-ons |
| `membrane-materialization` | `membrane-transport` | [MCP_MEMBRANE_GATEWAY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MCP_MEMBRANE_GATEWAY_PROPOSAL.md) | proposed | `docs/task.md` → membrane materialization and transport-home follow-ons |
| `webhook-security-contract` | `membrane-transport` | [TELEGRAM_INTEGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_INTEGRATION_PROPOSAL.md) | `docs/task.md` → Telegram integration follow-on work |
| `watched-live-telegram-validation` | `membrane-transport` | [TELEGRAM_INTEGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_INTEGRATION_PROPOSAL.md) | `docs/task.md` → Telegram watched-live follow-ons |
| `a2a-membrane-contract` | `membrane-transport` | [MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md) | `docs/task.md` → external membrane transport follow-ons |
| `nostr-membrane-contract` | `membrane-transport` | [MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md) | `docs/task.md` → Nostr communication-plane investigation and external membrane follow-ons |
| `transport-edge-trust-gates` | `membrane-transport` | [MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md) | `docs/task.md` → perimeter trust and membrane ingress hardening follow-ons |
| `membrane-sentinel-checks` | `membrane-transport` | [MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md) | `docs/task.md` → security finding, scanning, and membrane supervision follow-ons |
| `voice-transcribe-reentry` | `membrane-transport` | [VOICE_MACHINE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/VOICE_MACHINE_PROPOSAL.md) | `docs/task.md` → deferred voice/transcription follow-ons |
| `dedicated-voice-machine-component` | `membrane-transport` | [VOICE_MACHINE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/VOICE_MACHINE_PROPOSAL.md) | `docs/task.md` → voice-machine component follow-ons |
| `placement-policy-broadening` | `mesh-placement` | [INTER_HOTEL_ROUTING_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/INTER_HOTEL_ROUTING_PROPOSAL.md) | `docs/task.md` → `New Project: Inter-Hotel Routing And Placement` |
| `multi-host-watched-validation` | `mesh-placement` | [INTER_HOTEL_ROUTING_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/INTER_HOTEL_ROUTING_PROPOSAL.md) | `docs/task.md` → multi-host routing and VPS validation work |
| `multi-hotel-route-consistency` | `mesh-placement` | [MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md) | `docs/task.md` → `New Project: Multi-Hotel Component Distribution` |
| `cross-host-distributed-validation` | `mesh-placement` | [MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md) | `docs/task.md` → `New Project: Multi-Hotel Component Distribution` |
| `remote-materialization-ceremony` | `mesh-placement` | [MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md) | `docs/task.md` → `New Project: Multi-Hotel Component Distribution` |
| `capacity-relief-placement` | `mesh-placement` | [MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md) | `docs/task.md` → `New Project: Multi-Hotel Component Distribution` |
| `mesh-visible-state-contract` | `mesh-placement` | [MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md) | `docs/task.md` → `New Project: Mesh Visibility And State Placement` |
| `secret-handling-hardening` | `deployment-distribution` | [RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md) | `docs/task.md` → `Red Hat Ansible / VPS Deployment Boundary` |
| `watched-live-vps-smoke` | `deployment-distribution` | [RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md) | `docs/task.md` → VPS smoke follow-ons |
| `artifact-distribution-rollout` | `deployment-distribution` | [RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md) | `docs/task.md` → build/distribution follow-ons |
| `public-cli-naming` | `deployment-distribution` | [HOMEBREW_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HOMEBREW_DISTRIBUTION_PROPOSAL.md) | `docs/task.md` → `New Project: Homebrew Distribution` |
| `homebrew-release-pipeline` | `deployment-distribution` | [HOMEBREW_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HOMEBREW_DISTRIBUTION_PROPOSAL.md) | `docs/task.md` → `New Project: Homebrew Distribution` |
| `artifact-trust-contract` | `deployment-distribution` | [RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md) | `docs/task.md` → `Runner artifact plane` |
| `runner-release-distribution` | `deployment-distribution` | [RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md) | `docs/task.md` → `Runner artifact plane` |
| `egress-policy-object` | `operator-control-plane` | [PERIMETER_EGRESS_CONTROL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PERIMETER_EGRESS_CONTROL_PROPOSAL.md) | `docs/task.md` → `Perimeter egress control` |
| `outbound-classification` | `operator-control-plane` | [PERIMETER_EGRESS_CONTROL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PERIMETER_EGRESS_CONTROL_PROPOSAL.md) | `docs/task.md` → `Perimeter egress inventory` |
| `hotel-membership-records` | `operator-control-plane` | [HOTEL_PERIMETER_TRUST_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HOTEL_PERIMETER_TRUST_PROPOSAL.md) | `docs/task.md` → `New Project: Hotel Perimeter Trust` |
| `perimeter-authz-policy` | `operator-control-plane` | [HOTEL_PERIMETER_TRUST_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HOTEL_PERIMETER_TRUST_PROPOSAL.md) | `docs/task.md` → `New Project: Hotel Perimeter Trust` |
| `shell-runner-split` | `tooling-execution` | [TASK_RUNNER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TASK_RUNNER_PROPOSAL.md) | `docs/task.md` → `Next Project: Tool Assembly and Routed Execution` |
| `runner-materialization-policy` | `tooling-execution` | [TASK_RUNNER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TASK_RUNNER_PROPOSAL.md) | `docs/task.md` → `Tool runner lifecycle policy` |
| `unreachable-runner-fallback` | `tooling-execution` | [TASK_RUNNER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TASK_RUNNER_PROPOSAL.md) | `docs/task.md` → runner fallback/materialization follow-ons |
| `desktop-runner-materialization` | `tooling-execution` | [COMPUTER_USE_TASK_RUNNER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/COMPUTER_USE_TASK_RUNNER_PROPOSAL.md) | `docs/task.md` → CUA runner observe-only scaffold |
| `desktop-action-approval-policy` | `tooling-execution` | [COMPUTER_USE_TASK_RUNNER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/COMPUTER_USE_TASK_RUNNER_PROPOSAL.md) | `docs/task.md` → CUA action gating before input tools |
| `desktop-observation-contract` | `tooling-execution` | [COMPUTER_USE_TASK_RUNNER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/COMPUTER_USE_TASK_RUNNER_PROPOSAL.md) | `docs/task.md` → CUA screenshot/observe result contract |
| `agent-hook-registry` | `tooling-execution` | [AGENT_PLUGIN_HOOKS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_PLUGIN_HOOKS_PROPOSAL.md) | `docs/task.md` → `New Project: Agent Plugin Hooks` |
| `transcription-hook-extraction` | `tooling-execution` | [AGENT_PLUGIN_HOOKS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_PLUGIN_HOOKS_PROPOSAL.md) | `docs/task.md` → `New Project: Agent Plugin Hooks` |
| `route-readiness-checks` | `tooling-execution` | [TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md) | `docs/task.md` → `Next Project: Tool Assembly and Routed Execution` |
| `runner-fallback-policy` | `tooling-execution` | [TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md) | `docs/task.md` → `Next Project: Tool Assembly and Routed Execution` |
| `tool-management-plane-records` | `tooling-execution` | [TOOL_MANAGEMENT_PLANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_MANAGEMENT_PLANE_PROPOSAL.md) | `docs/task.md` → `Next Project: Tool Assembly and Routed Execution` |
| `agent-default-toolsets` | `tooling-execution` | [TOOL_MANAGEMENT_PLANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_MANAGEMENT_PLANE_PROPOSAL.md) | `docs/task.md` → `Next Project: Tool Assembly and Routed Execution` |
| `structured-model-envelope` | `tooling-execution` | [MODEL_CONTROLLER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MODEL_CONTROLLER_PROPOSAL.md) | `docs/task.md` → `New Project: Model Controller` |
| `hotel-gemini-oauth-flow` | `tooling-execution` | [MODEL_CONTROLLER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MODEL_CONTROLLER_PROPOSAL.md) | `docs/task.md` → `New Project: Model Controller` |
| `openai-provider-contract` | `tooling-execution` | [MODEL_CONTROLLER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MODEL_CONTROLLER_PROPOSAL.md) | `docs/task.md` → `New Project: Model Controller` |
| `hotel-openai-oauth-flow` | `tooling-execution` | [MODEL_CONTROLLER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MODEL_CONTROLLER_PROPOSAL.md) | `docs/task.md` → `New Project: Model Controller` |
| `provider-capability-overrides` | `tooling-execution` | [MODEL_CONTROLLER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MODEL_CONTROLLER_PROPOSAL.md) | `docs/task.md` → `New Project: Model Controller` |
| `provider-native-response-mode-routing` | `tooling-execution` | [MODEL_CONTROLLER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MODEL_CONTROLLER_PROPOSAL.md) | `docs/task.md` → `New Project: Model Controller` |
| `model-graph-decision-layer` | `tooling-execution` | [MODEL_GRAPH_AND_CONTEXT_1_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MODEL_GRAPH_AND_CONTEXT_1_PROPOSAL.md) | `docs/task.md` → `New Project: Model Controller / Model Graph Decision Layer` |
| `context-1-lookup` | `tooling-execution` | [MODEL_GRAPH_AND_CONTEXT_1_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MODEL_GRAPH_AND_CONTEXT_1_PROPOSAL.md) | `docs/task.md` → `New Project: Model Controller / Model Graph Decision Layer` |
| `capability-aware-tool-approval` | `runtime-sessions` | [MODEL_GRAPH_AND_CONTEXT_1_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MODEL_GRAPH_AND_CONTEXT_1_PROPOSAL.md) | `docs/task.md` → `New Project: Model Controller / Model Graph Decision Layer` |
| `context-engine-contract` | `memory-context` | [PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md) | `docs/task.md` → `New Project: Context And Memory Engines` |
| `deterministic-context-assembly` | `memory-context` | [PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md) | `docs/task.md` → `New Project: Context And Memory Engines` |
| `embeddinggemma-swap-validation` | `memory-context` | [EMBEDDINGGEMMA_SWAP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/EMBEDDINGGEMMA_SWAP_PROPOSAL.md) | `docs/task.md` → `New Project: EmbeddingGemma Swap` |
| `wider-client-adoption` | `memory-context` | [MUNINN_MEMORY_PROTOCOL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MUNINN_MEMORY_PROTOCOL_PROPOSAL.md) | `docs/task.md` → `Next Work Item: Muninn Heuristic Memory Experiment` |
| `philotic-native-memory-integration` | `memory-context` | [MUNINN_MEMORY_PROTOCOL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MUNINN_MEMORY_PROTOCOL_PROPOSAL.md) | `docs/task.md` → `Context And Memory Engines` and Muninn follow-ons |
| `memory-spacetime-frame` | `memory-context` | [MEMORY_CULTIVATION_TRUE_UP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMORY_CULTIVATION_TRUE_UP_PROPOSAL.md) | implemented-runtime | `docs/task.md` → `New Project: Memory Cultivation and True-Up` |
| `memory-shaping-context` | `memory-context` | [MEMORY_CULTIVATION_TRUE_UP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMORY_CULTIVATION_TRUE_UP_PROPOSAL.md) | implemented-runtime | `docs/task.md` → `New Project: Memory Cultivation and True-Up` |
| `memory-cultivation-loop` | `memory-context` | [MEMORY_CULTIVATION_TRUE_UP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMORY_CULTIVATION_TRUE_UP_PROPOSAL.md) | implemented-runtime | `docs/task.md` → `New Project: Memory Cultivation and True-Up` |
| `graph-muninn-true-up` | `memory-context` | [MEMORY_CULTIVATION_TRUE_UP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMORY_CULTIVATION_TRUE_UP_PROPOSAL.md) | implemented-runtime | `docs/task.md` → `New Project: Memory Cultivation and True-Up` |
| `memory-promotion-gates` | `memory-context` | [MEMORY_CULTIVATION_TRUE_UP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMORY_CULTIVATION_TRUE_UP_PROPOSAL.md) | implemented-runtime | `docs/task.md` → `New Project: Memory Cultivation and True-Up` |
| `memory-engine-contract` | `memory-context` | [MEMORY_ENGINE_ABSTRACTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMORY_ENGINE_ABSTRACTION_PROPOSAL.md) | `docs/task.md` → `New Project: Context And Memory Engines` |
| `graph-muninn-memory-dual-path` | `memory-context` | [MEMORY_ENGINE_ABSTRACTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMORY_ENGINE_ABSTRACTION_PROPOSAL.md) | `docs/task.md` → `New Project: Context And Memory Engines` |
| `life-graph-schema` | `memory-context` | [LIFE_GRAPH_OS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/LIFE_GRAPH_OS_PROPOSAL.md) | schema-applied-live | V001+V002+V003 live on Memgraph 3.10.1 vps-jane (25 constraints, 18 indexes, 25 vector indexes at 768d, StewardshipInstruction). No open backlog for this seam. |
| `life-graph-memorygraphrag-runner` | `memory-context` | [LIFE_GRAPH_OS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/LIFE_GRAPH_OS_PROPOSAL.md) | provider-handlers-green | life.observe live+verified, life.recall projection + named strategy dispatch done, life.commit/resolve/conflict/patch.propose provider handlers done. Open: hotel runtime → life.recall IPC invocation and IPC smoke. |
| `life-graph-attention-steward` | `memory-context` | [LIFE_GRAPH_OS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/LIFE_GRAPH_OS_PROPOSAL.md) | test-green-observe-policy | Paracrine subscriber observe-only policy + SIL spec + Beacon contract done. Open: active SIL entries and operator confirmation gate in philote. |
| `life-graph-agentic-growth-loop` | `memory-context` | [LIFE_GRAPH_OS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/LIFE_GRAPH_OS_PROPOSAL.md) | test-green-contracts | Patch gates, growth signals, drift categories, risk-tiered evaluation done. Open: growth-loop philote role; background drift detector job. |
| `life-graph-semantic-retrieval` | `memory-context` | [LIFE_GRAPH_OS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/LIFE_GRAPH_OS_PROPOSAL.md) | named-dispatch-green | 5 named strategies spec + contracts done; provider dispatches open_loops_by_context, goals_and_next_actions, commitments_approaching, re_entry_context, and cross_domain_entanglement. Open: retrieval quality logging + feedback path. |
| `life-graph-evidence-conflict` | `memory-context` | [LIFE_GRAPH_OS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/LIFE_GRAPH_OS_PROPOSAL.md) | provider-handlers-green | EvidencePacket + ConflictHandoff contracts done; provider handle_conflict/handle_resolve done. Open: runtime conflict detection + Muninn true-up/contradiction-review tool handoff. |
| `life-graph-paracrine-heartbeat` | `memory-context` | [LIFE_GRAPH_OS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/LIFE_GRAPH_OS_PROPOSAL.md) | test-green-runtime-boundary | Cron heartbeat → paracrine signal → philote observe path done. No open backlog for this seam. |
| `legacy-agent-import` | `migration-parity` | [ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md) | `docs/task.md` → `Next Project: Personality and Context` and migration follow-ons |
| `recognizable-identity-continuity` | `migration-parity` | [ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md) | `docs/task.md` → `Next Project: Personality and Context` and migration follow-ons |
| `parity-matrix` | `migration-parity` | [OPENCLAW_PARITY_MIGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/OPENCLAW_PARITY_MIGRATION_PROPOSAL.md) | `docs/task.md` → `New Project: OpenClaw Parity And Migration` |
| `migration-readiness-gates` | `migration-parity` | [OPENCLAW_PARITY_MIGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/OPENCLAW_PARITY_MIGRATION_PROPOSAL.md) | `docs/task.md` → `New Project: OpenClaw Parity And Migration` |
| `active-proposal-frontmatter-rollout` | `workflow-docs` | [PROPOSAL_ORGANIZATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PROPOSAL_ORGANIZATION_PROPOSAL.md) | `docs/task.md` → `Documentation Process And Architecture Truth` |
| `architecture-doc-metadata-rollout` | `workflow-docs` | [DOC_TAGGING_FRONTMATTER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DOC_TAGGING_FRONTMATTER_PROPOSAL.md) | `docs/task.md` → `Documentation Process And Architecture Truth` |
| `proposal-disposition-rollout` | `workflow-docs` | [AGENT_WORKFLOW_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_WORKFLOW_PROPOSAL.md) | `docs/task.md` → workflow/process follow-ons |
| `watched-live-recipe` | `workflow-docs` | [AGENT_WORKFLOW_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_WORKFLOW_PROPOSAL.md) | `docs/task.md` → watched-live recipe follow-ons |
| `engine-bootstrap-routine` | `workflow-docs` | [DEV_ENGINE_OPTIMIZATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DEV_ENGINE_OPTIMIZATION_PROPOSAL.md) | `docs/task.md` → `New Project: Dev Engine Optimization` |
| `reality-gap-consolidation` | `workflow-docs` | [DEV_ENGINE_OPTIMIZATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DEV_ENGINE_OPTIMIZATION_PROPOSAL.md) | `docs/task.md` → `New Project: Dev Engine Optimization` |
| `session-start-bootstrap-slice` | `workflow-docs` | [DEV_ENGINE_OPTIMIZATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DEV_ENGINE_OPTIMIZATION_PROPOSAL.md) | `docs/task.md` → `New Project: Dev Engine Optimization` |
| `admin-posture-model` | `operator-control-plane` | [ROLE_POSTURE_AND_ADMIN_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROLE_POSTURE_AND_ADMIN_PROPOSAL.md) | `docs/task.md` → `New Project: Admin Role And Surfaces` |
| `session-admin-elevation` | `operator-control-plane` | [ROLE_POSTURE_AND_ADMIN_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROLE_POSTURE_AND_ADMIN_PROPOSAL.md) | `docs/task.md` → `New Project: Admin Role And Surfaces` |
| `cli-tui-admin-surface` | `operator-control-plane` | [CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md) | `docs/task.md` → `New Project: Admin Role And Surfaces` |
| `action-grant-contract` | `operator-control-plane` | [CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md) | `docs/task.md` → `New Project: Admin Role And Surfaces` |
| `local-admin-capability-envelope` | `operator-control-plane` | [LOCAL_ADMIN_FALLBACK_MODEL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/LOCAL_ADMIN_FALLBACK_MODEL_PROPOSAL.md) | `docs/task.md` → `New Project: Local Admin Fallback Model` |
| `onnx-admin-fallback-path` | `operator-control-plane` | [LOCAL_ADMIN_FALLBACK_MODEL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/LOCAL_ADMIN_FALLBACK_MODEL_PROPOSAL.md) | `docs/task.md` → `New Project: Local Admin Fallback Model` |
| `observability-event-envelope` | `operator-control-plane` | [ROUTER_NATIVE_OBSERVABILITY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROUTER_NATIVE_OBSERVABILITY_PROPOSAL.md) | `docs/task.md` → `New Project: Router-Native Observability` |
| `attachable-observability-listeners` | `operator-control-plane` | [ROUTER_NATIVE_OBSERVABILITY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROUTER_NATIVE_OBSERVABILITY_PROPOSAL.md) | `docs/task.md` → `New Project: Router-Native Observability` |
| `vault-secret-refs` | `operator-control-plane` | [KEY_VAULT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/KEY_VAULT_PROPOSAL.md) | `docs/task.md` → `New Project: Key Vault` |
| `remote-vault-delegation` | `operator-control-plane` | [KEY_VAULT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/KEY_VAULT_PROPOSAL.md) | `docs/task.md` → `New Project: Key Vault` |
| `elevenlabs-streaming-tts` | `tooling-execution` | [STREAMING_TTS_AND_MUSIC_ANALYSIS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/STREAMING_TTS_AND_MUSIC_ANALYSIS_PROPOSAL.md) | `docs/task.md` → `New Project: Streaming TTS And Music Analysis` |
| `elevenlabs-stt-surface` | `tooling-execution` | [STREAMING_TTS_AND_MUSIC_ANALYSIS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/STREAMING_TTS_AND_MUSIC_ANALYSIS_PROPOSAL.md) | `docs/task.md` → `New Project: Streaming TTS And Music Analysis` |
| `onnx-music-analysis-surface` | `tooling-execution` | [STREAMING_TTS_AND_MUSIC_ANALYSIS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/STREAMING_TTS_AND_MUSIC_ANALYSIS_PROPOSAL.md) | `docs/task.md` → `New Project: Streaming TTS And Music Analysis` |
| `midi-output-artifact` | `tooling-execution` | [STREAMING_TTS_AND_MUSIC_ANALYSIS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/STREAMING_TTS_AND_MUSIC_ANALYSIS_PROPOSAL.md) | `docs/task.md` → `New Project: Streaming TTS And Music Analysis` |
| `graph-harness-control-plane` | `operator-control-plane` | [AGENT_WORKSTREAM_TRACKING_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_WORKSTREAM_TRACKING_PROPOSAL.md) | `docs/task.md` → Harness desired/rendered/observed state management in intel-graph |

## Usage Rule

When adding a new `active_seams` entry to a proposal:

1. add the seam ID here first
2. link it to one primary proposal
3. note the current `docs/task.md` surface where execution lives
4. only create a dedicated seam doc when the seam becomes large enough to need its own boundary narrative

## Current Transitional Note

For now, seam IDs are canonical in this registry and referenced from proposal frontmatter. `docs/task.md` still uses human-readable section structure rather than seam-ID-first headings. That is intentional until the task surface proves it needs tighter machine-like linkage.
