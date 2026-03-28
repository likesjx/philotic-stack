---
title: "Seam Roadmap"
doc_type: reference
domain: governance
status: active
last_updated: 2026-03-26
tags:
  - roadmap
  - seams
  - dependencies
related_docs:
  - SEAM_REGISTRY.md
  - ARCH_RULES_AND_ROADMAP_PROPOSAL.md
---

# Seam Roadmap

Dependency-ordered view of implementation seams across active proposals.

**This is not a task list.** Tasks live in [docs/task.md](../../docs/task.md).
This answers: "if I want to build X, what foundation must exist first?"

Only seams where cross-proposal dependency ordering matters are listed here.
Single-proposal seams with no cross-cutting deps are tracked only in SEAM_REGISTRY.md.

See [ARCH_RULES_AND_ROADMAP_PROPOSAL.md](ARCH_RULES_AND_ROADMAP_PROPOSAL.md) for the full process.

---

## Graph Layer

| seam_id | source_proposal | depends_on | status | summary |
|---|---|---|---|---|
| `graph-domain-layer` | `GRAPH_LAYER_UNIFICATION_PROPOSAL` | — | not-started | Introduce `GraphDomain` as the required call surface for all domain graph operations; no direct `GraphStorage` backend calls from domain code |
| `graph-adapter-migration` | `GRAPH_LAYER_UNIFICATION_PROPOSAL` | `graph-domain-layer` | not-started | Migrate existing `GraphStorage` callers in `aiua`, `philote`, and `ansible-mesh-core` to call through `GraphDomain` |
| `graph-store-instances` | `GRAPH_LAYER_UNIFICATION_PROPOSAL` | `graph-adapter-migration` | not-started | Make storage backend a deployment-time config choice; prove backend swap compiles and passes tests without caller changes |

---

## Agent Resource Model

| seam_id | source_proposal | depends_on | status | summary |
|---|---|---|---|---|
| `agent-resource-broker` | `AGENT_RESOURCE_MODEL_PROPOSAL` | `graph-domain-layer` | not-started | Resource registry in `aiua` separate from `GuestManager`; `ResourceRequest` / `ResourceGranted` / `ResourceDenied` / `ResourceRevoked` IPC types; routing table (resource→tenants, agent→resources) |
| `demand-derived-materialization` | `AGENT_RESOURCE_MODEL_PROPOSAL` | `agent-resource-broker` | not-started | `static_resource_declarations` on agent ODS records; boot reconciliation replaces static guest config; suspend-vs-remove distinction; teardown on zero tenants |
| `agent-graph-toolrunner` | `AGENT_RESOURCE_MODEL_PROPOSAL` | `agent-resource-broker`, `graph-domain-layer` | not-started | `AgentGraphStorage` trait; `SqliteAgentGraphStorage` impl; `AgentGraph` resource type; tool-runner variant exposing `agent.graph.*` surface; hotel materializes this before agent is ready |
| `agent-graph-mesh-sync` | `AGENT_RESOURCE_MODEL_PROPOSAL` | `agent-graph-toolrunner` | not-started | Export/import surface on `AgentGraphStorage`; wire into existing mesh CRDT transport; LWW conflict resolution; two-tier authority invariant tests |
| `router-training-tap` | `AGENT_RESOURCE_MODEL_PROPOSAL` | `agent-resource-broker` | not-started | Router observability tap; `RoutedMessage` identity context fields on existing IPC types; router-listener as hotel system resource; append-only `ExperienceTrace` store |
| `functions-gemma-onnx` | `AGENT_RESOURCE_MODEL_PROPOSAL` | `router-training-tap`, `agent-resource-broker` | not-started | FunctionsGemma as ONNX runner Slice 3; registered as requestable resource type; RL training pipeline reads from trace store; hot-swap support |

---

## Cross-Proposal Dependencies (Summary)

```
graph-domain-layer
  └─ graph-adapter-migration
       └─ graph-store-instances

graph-domain-layer ──────────────┐
                                 ▼
agent-resource-broker ──────► agent-graph-toolrunner
  │                                └─ agent-graph-mesh-sync
  ├─► demand-derived-materialization
  └─► router-training-tap
           └─ functions-gemma-onnx
```

The graph layer must land before the agent resource model — `agent-graph-toolrunner` builds on `GraphDomain` as its storage abstraction, and the migration seam proves the abstraction holds across callers before the agent graph extends it.

---

## Completed Seams

_Seams are moved here when their status becomes `complete`._

| seam_id | source_proposal | completed | summary |
|---|---|---|---|
| `telegram-poll-lease` | `TELEGRAM_POLL_LEASE_PROPOSAL` | 2026-03 | Poll lease acquire, renew, expiry, home-hotel checks, graceful release, delegated remote polling |
| `runtime-authority-leases` | `RUNTIME_AUTHORITY_LEASES_PROPOSAL` | 2026-03 | Shared `LeaseEnvelope` and central runtime lease registry; Telegram lease migrated onto abstraction |

---

## Roadmap Maintenance

- When a seam is added to SEAM_REGISTRY with cross-proposal deps, add it here.
- When a seam completes, move it to the Completed table.
- When a proposal is superseded, annotate affected seams with a `→ superseded-by` note before moving or removing them.
- Dependency arrows in the summary diagram should match `depends_on` entries in the tables.
