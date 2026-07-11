---
title: Memory Transparency — One Provenance Envelope, One Explain Surface
doc_type: proposal
domain: memory-context
status: active
disposition: proposed
last_updated: 2026-07-11
tags:
- memory
- provenance
- transparency
- muninn
- lifegraph
- audit
related_docs:
- AUTOPOIESIS_PROPOSAL.md
- SUBSTRATE_HARDENING_PROPOSAL.md
- LIFE_GRAPH_OS_PROPOSAL.md
- MEMORY_CULTIVATION_TRUE_UP_PROPOSAL.md
- MUNINN_MEMORY_PROTOCOL_PROPOSAL.md
task_refs:
- docs/task.md
---

# Memory Transparency — One Provenance Envelope, One Explain Surface

> Fully transparent memory means the operator can watch the system's mind
> change — see what was learned, why, from what evidence — and reverse any
> change. Memory writes are actions; they carry the same
> auditable/reversible/budgeted contract as every other autonomous action.

## Goal

Philotic has three memory planes with distinct authority:

- **Muninn** — cognitive continuity (decisions, reality gaps, preferences)
- **Intel graph** — work structure (proposals, seams, decisions, verification)
- **LifeGraph** — operator-world effects (entities, signals, aspirations)

Muninn already carries the transparency primitives — `trust` tiers,
`provenance`, `explain`, `contradictions`, soft-delete/restore. The other two
planes carry fragments (intel-graph decision records; LifeGraph provenance in
the truth-summarizer skill's vocabulary). Transparency today is therefore
*per-plane and partial*: no single query answers "why does the system believe
X?", promotions between planes lose lineage, and memory changes happen with no
operator-visible ledger.

## Core Recommendation

Standardize one **provenance envelope** on every memory write in every plane,
then build the two surfaces that consume it: a merged *explain* query and an
operator-facing *memory-delta digest*. Add a `memory.hygiene` autonomy lane so
pruning is scheduled and audited, not aspirational.

```
ProvenanceEnvelope {
  source:   turn/event/session id that caused the write,
  author:   agent + role (or scanner/system component),
  trust:    observed | inferred | told   (Muninn's existing tiers, adopted fleet-wide),
  evidence: pointer(s) — log lines, graph nodes, LifeGraph signals, PR/commit,
  reversal: the named undo path (soft-delete id, revert edge, restore point)
}
```

## Slices

| Slice | Content | Size | Verify |
|---|---|---|---|
| M1 `provenance-envelope` | Define the envelope as a shared type (ansible-mesh-core), adopt it on all three write paths: Muninn writes map their existing fields onto it; intel-graph decision records gain the missing fields (evidence pointer, reversal path); LifeGraph mutations (life.observe, patches, steward writes) attach it. Cross-plane promotions (seam:lifegraph-muninn-promotion) carry the envelope through so lineage survives the hop. | M–L | test-green + one memory traced across a Muninn→LifeGraph promotion with intact lineage |
| M2 `explain-surface` | One query — "why do you believe X?" — that fans out to muninn_explain + intel-graph decision trail + LifeGraph evidence and returns a merged provenance chain, separating confirmed facts / seeded placeholders / inferred intent (the lifegraph-truth-summarizer skill's vocabulary, promoted from agent instructions to a runtime tool any philote or the operator can invoke). Exposed via IPC tool + `phil memory explain`. | M | test-green + live query on a real belief returns all three planes' evidence |
| M3 `memory-delta-digest` | Extend the architect-charter morning dev-brief: what the fleet remembered, evolved, forgot, and found contradictory yesterday — each line linking to its envelope, each reversible from the digest. This is the operator's window on the system's changing mind; without it, transparency exists but is never looked at. | S–M | watched-live (first digest reviewed by operator; one item reversed from it) |
| M4 `memory.hygiene` lane | Nightly per-hotel consolidation cadence: muninn_consolidate, contradiction sweep (muninn_contradictions), staleness annotation, graph_memory_true_up. Findings that need judgment file proposals (the autopoiesis A3 filing pattern pointed at memory instead of guests). Runs as an AutonomyGrant lane: ProposalOnly for anything destructive, AutoWithAudit only ever for annotation/flagging. Learning is not just accumulation — nothing currently prunes. | M | smoke-green (seeded contradiction is swept, annotated, and filed) + first live nightly cycle audited |

Dependency: M1 → {M2, M3}; M4 independent of M1 (uses existing Muninn
primitives) but its filings get richer once M1 lands. M3 rides the existing
architect-charter cron (autopoiesis A4). The LifeGraph retrieval lane's Slice 4
(Muninn provenance) is M1's direct ancestor — build on it, don't duplicate it.

## Standing Rules

1. **No naked writes:** a memory write without a provenance envelope is a
   defect in the writing component, in any plane.
2. **Trust tiers are honest:** `observed` requires evidence pointers;
   `inferred` and `told` are never silently upgraded — only an explicit
   confirmation event upgrades trust.
3. **Every forget is reversible for a window:** destructive memory operations
   go through soft-delete with the restore path named in the envelope.
4. **Transparency is pulled AND pushed:** the explain surface answers on
   demand; the digest pushes deltas daily. Both exist or neither matters.

## Disposition

`proposed` — authored 2026-07-11 from the autopoiesis roadmap assessment.
M4 (hygiene lane) and M1 (envelope) can start immediately and in parallel.
