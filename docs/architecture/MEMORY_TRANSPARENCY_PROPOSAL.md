---
title: Memory Transparency — One Provenance Envelope, One Explain Surface
doc_type: proposal
domain: memory-context
status: active
disposition: accepted for current slice
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
| M4 `memory.hygiene` lane | **First slice landed** (`codex/memory-m4-hygiene-lane`): nightly per-hotel sweep — `GET /api/contradictions` and an age-based staleness proxy (`GET /api/engrams?sort=created&before=...`; MuninnDB's public REST has no `last_accessed` field, so this is `created_at` age, not true access recency — see the module doc in `crates/aiua/src/memory_hygiene.rs`). Findings crossing threshold file ONE aggregated `autonomy_audit` record on the `memory.hygiene` lane (annotation only — no `forget`/`consolidate` call); every run, filed or clean, also gets a lightweight per-run marker (hotel-scoped config value, not budget-gated) so "what was scanned" stays visible even on clean nights. Scoped out of this slice: `muninn_consolidate` (destructive by definition — excluded from M4's non-destructive contract on purpose) and `graph_memory_true_up` (MCP-only on the graph-intelligence server, logic lives in the `philote` guest — not callable in-process from `aiua`). Registered as a `CronJob` whose fire is intercepted by `CronTicker` before guest delivery; opt-in per hotel via `PHILOTIC_MEMORY_HYGIENE_ENABLED`, re-checked at fire time (not just registration) because mesh `CronJobSync` replicates job *definitions* to every peer hotel unconditionally — without the fire-time re-check, one hotel's opt-in would silently sweep every mesh-connected peer. Filing itself additionally gated by the lane's `AutonomyGrant` kill switch/budget (`PHILOTIC_AUTONOMY_DISABLE_MEMORY_HYGIENE`). | M | test-green (25 new unit tests: threshold/aggregation logic, lane filing/kill-switch/budget, per-run marker, idempotent cron registration, mesh-replication-does-not-leak-execution regression; `ansible-mesh-core`'s existing lane-enumeration tests extended to cover the new lane) — no live nightly cycle audited yet |

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

`accepted for current slice` — authored 2026-07-11 from the autopoiesis
roadmap assessment. M4's first slice (contradiction + age-based staleness
sweep, aggregated annotation-only filing, opt-in nightly cron) landed
2026-07-11 on `codex/memory-m4-hygiene-lane`; see the M4 row above for what
shipped and what was scoped out. M1 (envelope) has not started; M4's filings
stay evidence-light until it lands, per the dependency note above.
