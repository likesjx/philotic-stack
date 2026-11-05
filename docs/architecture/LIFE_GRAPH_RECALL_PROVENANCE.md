---
title: LifeGraph recall provenance in Philote
doc_type: proposal
domain: memory-context
status: accepted-current-slice
last_updated: 2026-11-05
tags: [memory, provenance, life-graph, philote]
related_docs:
- AUTOMATIC_RECALL_STATUS.md
- ARCHITECTURE_STATUS.md
- NATIVE_GRAPH_TOOLS_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: life-graph-recall-provenance
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# LifeGraph recall provenance in Philote

## Goal

Preserve the existing LifeGraph evidence/authority boundary through automatic
Philote enrichment, rather than reducing a ranked packet to prose and confidence.

## Core recommendation

Add optional `RecalledMemoryRecord.life_graph_provenance`, using the existing
`RankedEvidencePacket` contract plus its retrieval context/query IDs, strategy,
generation timestamp and omitted-conflict references. This preserves source and
passage references, reliability basis, validation/adjudication, valid-time range,
claim metadata/properties, matched policy filters and graph evidence path.
Muninn records remain unchanged. Old records deserialize with no new field.

The `EvidencePacket.context_authority()` helper extracts the exact existing
ContextPacket rule; both consumers use it. Only stored `Confirmed` validation
maps to LifeGraph truth. Confidence, retrieval rank, source reliability, operator
source kind and Muninn continuity references never promote proposed/inferred,
retired or conflicted evidence. Source refs retain their own types and reliability.
The adapter does not interpret arbitrary metadata authority claims as validation.
Prompt hints explicitly retain the ContextPacket rule that Muninn refs are
continuity handles, not confirmation of LifeGraph truth.
The existing observed timestamp remains the record's created timestamp; retrieval
generation time remains separately visible, not substituted for event freshness.

Record deduplication now requires matching complete evidence, policy filters,
evidence paths and omitted-conflict references as well as duplicate ID or normalized content. A Muninn copy and a
provenance-bearing LifeGraph copy remain separate; different source, validation,
scope filter or evidence path cannot disappear merely because prose or IDs match.
Equivalent evidence from multiple strategies still collapses; the first retrieval
receipt wins, so this is not a ledger of every query that returned the same evidence.
Legacy records lacking provenance on both sides retain ID/content deduplication.
The protected request-local recall selector uses the same equivalence helper,
after its existing authority check and before complete-record byte admission.
No protected catalog, manifest binding or permission predicate is relaxed.
Omission-only differences remain separate records with their original retrieval
references; no union, confidence/authority merge or write-back occurs.

The existing complete-record metadata renderer includes the new typed field and
explicit derived authority/qualification hints. Claim prose renders once, while
all other stored provenance remains complete. Conflict references warn against
inferring resolution from recall; adjudication state remains visible. Private
payloads are prompt evidence for the already authorized turn, never log fields.

Both serialized cache admission and final prompt UTF-8 byte budgets include the
added provenance. A complete record fits or is omitted whole; evidence qualifiers
are never trimmed to squeeze in a claim. This can reduce the number of displayed
records. Matched remains matched before budgeting. Existing successful empty
packets stay empty/no-match, and stale-cache rules and retrieval scopes remain
unchanged. The adapter does not create new retrieval permissions, synchronously
search either store, or claim absent records are globally absent.

## Disposition

Implemented locally on `codex/life-recall-provenance`, based on develop merge
`4c30a79f`. Local regression qualification: 695 Philote, 44 memory-core, 228 graph-contract
and 57 LifeGraph runner/provider tests passed. Three existing actual-process
protected acceptance tests also pass (the child fixture is invoked by its parents). Independent review and any
publication are coordinated separately. Formatting, YAML and diff checks pass;
affected-crate Clippy passes with existing repository warnings.
No deployment, credentials, migration, deletion, new
gateway or change to candidate selection/promotion authority.

## Current slice

Tests compose real contracts, response handler, cache admission and model-context
projection with synthetic packet data and a framed-UDS test hotel. They cover all
validation states against canonical ContextPacket authority, full evidence and
checkpoint round-trip, conflicting source/scope/validation under equal prose/IDs,
no-match, stale cache, UTF-8 boundary and prompt budgets, and private-payload trace
canaries. This is not production Memgraph/Muninn or installed-runtime qualification;
retrieval/provider semantics are unchanged. The legacy `memory.explain`
best-effort ProvenanceEnvelope summary is unchanged; this adaptation preserves
typed evidence on recalled records and in reasoning prompts. Resource use reuses the existing
worktree and command-local shared Cargo target with incremental compilation off.
