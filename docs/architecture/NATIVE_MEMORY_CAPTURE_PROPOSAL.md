---
title: Native Memory Capture Through the Membrane
doc_type: proposal
domain: memory-context
status: accepted-current-slice
last_updated: 2026-10-29
tags: [native-memory, capture, delegation, membrane]
related_docs: [ARCHITECTURE_STATUS.md, MCP_MEMBRANE_GATEWAY_PROPOSAL.md, INTER_HOTEL_ROUTING_PROPOSAL.md]
task_refs: [../task.md]
---

# Goal

Capture a scoped original observation and verify recall through external membrane authentication → normalized signed request → hotel router → destination native Rust memory runner. First client is Percival/dot; Claude follows with the same contract. Do not restore unrelated agents as a prerequisite.

## Core Recommendation

Reuse membrane bearer checking and `EmitTask`, add an explicit `NativeMemory` target, and verify narrowly delegated authority again at the destination. Reusable credentials remain local. Short-lived signatures do not replace protected transport. Store source observations with stable IDs and provenance before adding semantic enrichment. Memgraph is the required live backend. SQLite is retained only as a synthetic contract-test fixture; no SQLite activation is planned.

## Disposition / Current Slice

Accepted for this bounded capture slice. Implementation includes real signature verification, private remember/recall, SQLite receipts/replay protection, opt-in membrane and native runner entrypoints, two-hotel simulation and real-binary IPC smoke. Not deployed, integrated with actual aiua hotels, connected to dot, or qualified for production. Source baseline `adfe7497`; original unrelated Telegram work is untouched. Prototype commit `b40694e9` was not present in available repository objects; existing protected-authority and execution-transport source was inspected rather than claiming reuse of an unavailable revision.

## Scope register preserved

| ID | Current status |
|---|---|
| R01 Native cognition | Separate 35-test toy experiment retained; not part of capture runtime |
| R02 Cypher/MAGE | Capability contract retained; no database graph algorithm implemented here |
| R03 Canonical evidence/dedup | Stable observation/source/lineage retained; canonical merge and sharing deferred |
| R04 Authority/disclosure | Signed delegation and local owner/namespace enforcement tested; protected transport and runtime integration outstanding |
| R05 Vault custody | No reusable secrets routed; real key custody/provisioning remains release gate |
| R06 Native runner | Remember/recall entrypoints implemented; actual dot/Claude connectors not qualified |
| R07 Memory plane | LifeGraph people/goals/roles/commitments, Chroma chunks, clean Obsidian originals joined by stable IDs/provenance remain full target |
| R08 Web governance | Later |
| R09 Lifecycle | Observation capture only; no fact/goal modification, consolidation or forgetting |
| R10 Evidence | Deterministic tests and disposable binary IPC smoke; no production/performance or power-loss qualification |
| R11 Transition | Parked; no personal-memory migration |
| R12 Sharding | Conditional, no implementation |
| R13 Feature inventory | NLP extraction, semantic links, graph embeddings and sleep consolidation explicitly later |
| R14 Receipts/decisions | Atomic local operation receipts/replay records; remote operational qualification later |
| R15 Semantic enrichment | Future semantic/NLP extraction, relevance and graphRAG qualification; not part of source capture |
| R16 Obsidian originals | Future original-document ingestion/reconciliation with stable IDs/provenance; no personal vault accessed |

## Concrete rollout effects — not authorized by publication alone

Proposed artifacts: rebuilt `membrane-mcp` at the chosen ingress hotel (intended VPS ingress) and new `native-memory` guest at the chosen destination (intended mbp-jane). New endpoint tool names: `native-memory.remember`, `native-memory.recall`, exposed through the existing MCP `/mcp` endpoint only after explicit configuration. No endpoint is provisioned by this PR.

Before any production step, agree exact installed binary paths, service names, local socket, Memgraph database/schema and local Bolt configuration, source namespace, issuer/key identity, destination/guest IDs and connector destination. Activation requires a local signing key at ingress, public-key pin plus narrow delegation grants at destination, native endpoint grants/preapproval, persistent Memgraph observation/receipt/replay storage and process installation/start/reload. These are specific persistent credential/grant/private-data effects requiring action-time approval. Protected transport and response integrity must be qualified first; no security/network settings are changed here. Parent found no eligible callable dot memory connector; do not call engineering tests dot acceptance.

Rollback: disable native endpoint routes and issuer grants, stop only the new native guest, restore the previous membrane artifact/config if changed, retain private graph observations and receipts for reconciliation. Do not delete observations or receipts as rollback. Existing Muninn-backed tools and unrelated agents remain unchanged. No merge/deployment is implied by the draft PR.

## Evidence boundaries and next work

See crate README for commands, operation schema and limitations. Publication/CI evidence is reported with exact SHA. Next local qualification is actual disposable aiua hotel composition plus protected transport and supported dot connector setup; live capture only after those gaps and specific activation effects are approved. Real LifeGraph persistence comes later with its own schema/transaction tests. No paid model/provider calls, source-derived Muninn translation or legal-clearance claim.

## Memgraph correction

The initial SQLite fixture isolated auth/durability tests; it was not a technical Memgraph limitation. The native adapter now uses existing neo4rs and a single verified Memgraph transaction for observations, event receipts and nonces. The disposable graph CI test must pass; WAL/restart durability and actual hotel/client composition remain separate acceptance gates. SQLite tests cannot substitute for these.
