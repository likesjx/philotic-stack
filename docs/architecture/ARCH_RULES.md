---
title: "Architectural Rule Registry"
doc_type: reference
domain: governance
status: active
last_updated: 2026-03-29
tags:
  - rules
  - governance
  - architecture
related_docs:
  - ARCH_RULES_AND_ROADMAP_PROPOSAL.md
---

# Architectural Rule Registry

Standing constraints extracted from accepted and implemented proposals.
Rules here are in force. Violating a `hard` rule is a defect.

**Process**: When a proposal reaches `accepted` or `implemented`, the author extracts rules here.
Rules are never deleted — superseded rules get a `superseded-by` note.
See [ARCH_RULES_AND_ROADMAP_PROPOSAL.md](ARCH_RULES_AND_ROADMAP_PROPOSAL.md) for the full process.

---

## Active Rules

| rule_id | domain | source | rule | level | applies_to |
|---|---|---|---|---|---|
| `hotel-cg-canonical-session-authority` | `runtime-sessions` | `SESSION_LOOP_PROPOSAL` | The hotel context graph is the canonical durable owner for session state; apartments are derived recovery projections, not a competing source of truth. | hard | any code that writes or reads session state |
| `one-canonical-state-owner` | `runtime-sessions` | `AGENTS.md §2.1` | When a kind of state exists, preserve one authority; if two places appear to own the same thing, resolve the boundary before extending behavior. | hard | all new state additions |
| `resource-requests-through-broker` | `runtime-sessions` | `AGENT_RESOURCE_MODEL_PROPOSAL` | Resource requests from agents must flow through the hotel resource broker, not be self-granted. | hard | all new resource acquisition paths in `philote` and guest processes |
| `agent-no-hotel-cg-write` | `runtime-sessions` | `AGENT_RESOURCE_MODEL_PROPOSAL` | Agents may not write to the Hotel CG directly; only hotel processes may write hotel-authority state. | hard | any IPC handler that accepts agent-originated writes |
| `hotel-cg-wins-on-grant-conflict` | `runtime-sessions` | `AGENT_RESOURCE_MODEL_PROPOSAL` | When Hotel CG and agent graph disagree on a grant, the Hotel CG wins. | hard | agent graph sync and grant resolution logic |
| `lease-at-resource-not-agent` | `membrane-transport` | `AGENT_RESOURCE_MODEL_PROPOSAL` | Leases (Telegram poll, desktop membrane, etc.) live at the resource instance level, not the agent level; one lease holder per resource regardless of tenant count. | hard | any new lease acquisition or renewal path |
| `poll-lease-anchored-to-home-hotel` | `membrane-transport` | `TELEGRAM_POLL_LEASE_PROPOSAL` | Telegram poll lease authority is anchored to the agent's home hotel, not the currently routed role. | hard | poll lease acquire, renew, and delegation logic |
| `routed-messages-carry-identity-context` | `tooling-execution` | `AGENT_RESOURCE_MODEL_PROPOSAL` | Router-observable messages must carry `agent_id`, `session_id`, and `active_role` for training reconstruction. | hard | all new IPC message types that flow through the hotel router |
| `graph-domain-layer` | `runtime-sessions` | `GRAPH_LAYER_UNIFICATION_PROPOSAL` | All domain graph operations must go through `GraphDomain`, not directly against the storage backend. | hard | all new storage consumers; any code calling `GraphStorage` directly |
| `storage-backend-is-deployment-detail` | `runtime-sessions` | `GRAPH_LAYER_UNIFICATION_PROPOSAL` | The storage backend (`GraphStorage` impl) is a deployment-time choice, not a caller concern; callers hold `Arc<dyn XxxStorage>` and must not downcast. | hard | all new `GraphStorage` consumers and trait impls |
| `transitional-architecture-must-be-named` | `workflow-docs` | `AGENTS.md §2.3` | Transitional architecture choices must be explicitly labeled as transitional in docs and close-out notes; scaffolding must not quietly become implied final architecture. | guidance | any slice that introduces a known-temporary pattern |
| `proven-inferred-intended-are-distinct` | `workflow-docs` | `AGENTS.md §2.4` | Keep a clear distinction between proven behavior, inferred behavior, and intended future design; do not collapse those categories in explanations, docs, or validation claims. | guidance | doc updates, PR descriptions, session notes |
| `tool-projection-is-policy` | `tooling-execution` | `AGENTS.md §5.3.2` | Tool availability is not the same as tool appropriateness; suppress high-agency tools on low-intent turns rather than passively mirroring all bindings. | guidance | any change to the tool projection surface in `philote` |
| `graph-is-canonical-source-of-truth` | `workflow-docs` | `GRAPH_INTELLIGENCE_PROPOSAL` | The SQLite graph is the canonical source of truth for process state; markdown files are human-readable projections, not authorities. | hard | any code that reads or writes proposal status, seam state, or task assignments |
| `agents-mutate-via-graph-tools` | `workflow-docs` | `GRAPH_INTELLIGENCE_PROPOSAL` | Agents must mutate architecture state via graph MCP tools (`graph_update_node`, `graph_create_edge`, `graph_decide`); direct file editing of frontmatter status fields is prohibited. | hard | agent implementations, automation scripts |
| `graph-writeback-is-optional` | `workflow-docs` | `GRAPH_INTELLIGENCE_PROPOSAL` | Synchronization from graph to markdown (`graph_writeback`) is explicit and optional; the graph state is authoritative even if writeback has not occurred. | guidance | agent workflows, documentation processes |
| `shared-fields-only-mutated` | `workflow-docs` | `GRAPH_INTELLIGENCE_PROPOSAL` | Only `status`, `last_updated`, `active_seams`, and `implemented_by` frontmatter fields may be mutated by the graph; all other fields remain under human editorial control. | hard | graph writeback implementation, agent tools |

---

## Superseded Rules

_None yet._

---

## Adding a Rule

1. Check that the source proposal has reached `accepted` or `implemented` disposition.
2. Assign a unique kebab-case `rule_id` — do not recycle old IDs.
3. Use exactly one sentence for `rule`. If you need two sentences, split into two rules.
4. Choose `hard` only if silently violating the rule would be a defect. When in doubt, use `guidance`.
5. Run `$architecture-docs-maintainer` after adding rows to verify cross-links.
