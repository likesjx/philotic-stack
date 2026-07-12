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
| M1 `provenance-envelope` | **Landed** (`codex/memory-m1-provenance-envelope`): shared `ProvenanceEnvelope { source, author, trust, evidence, reversal }` type in `ansible_mesh_core::provenance` (all fields `#[serde(default)]`, builder ctors `from_agent`/`from_component`). Adopted on: (1) autonomy audit ledger — `AutonomyAuditRecord.provenance: Option<ProvenanceEnvelope>`, populated at the A3 heal-filing IPC handler and the M4 hygiene sweep's filing call; (2) intel-graph decision records — `DecideBody` gained `evidence`/`reversal`/`trust` fields (plain JSON, no new crate dependency), stored on the decision node's `properties` and the `Mutation.details`, sent by both A3's and M4's `push_intel_graph_record`; (3) Muninn writes — `philote`'s `memory.remember` tool attaches the envelope into the `metadata` JSON already accepted by MuninnDB's `/api/engrams` (`trust: Told`, since the model asserted the fact via the tool call); (4) LifeGraph mutations — `LifeObserveInput.provenance`, populated by the paracrine attention-steward write path (`data-memorygraphrag::attention_observer`) and threaded all the way to the compiled Cypher (`n.provenance_envelope` on the Memgraph node, via `cypher::compile_observe` → `provider.rs`'s bound param). **Scoped out, named precisely**: the model-invoked `life.observe` tool-call path and the hand-rolled `direct_life_observe_input` JSON builder in `philote::memory_integration` do not yet inject an envelope (no analogous injection point to the existing `inject_scoped_to_anchor` hook — next seam). Cross-plane promotion (`seam:lifegraph-muninn-promotion`) is **not implemented anywhere in this repo** today — only comments in `cypher.rs`/`projection.rs` reserve fields for it — so there is nothing to carry the envelope through yet; this is a real gap, not a smeared one. | M–L | test-green: `ansible-mesh-core` provenance module (5 tests), autonomy/A3/M4/graph-intelligence/data-memorygraphrag suites extended (round-trip, backward-compat-with-absent-field, and one proof-of-adoption test per wired plane) — no live watched run |
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
shipped and what was scoped out. M1 (envelope) **landed** 2026-07-11 on
`codex/memory-m1-provenance-envelope`; see the M1 row above for the plane-by-
plane adoption detail. M4's filings now carry provenance on both the
hotel-graph audit ledger and the intel-graph mirror. Reality gaps this slice
surfaced: (a) the model-invoked `life.observe` tool call and the hand-rolled
`direct_life_observe_input` JSON path have no provenance-injection point yet
— `tool_exec::inject_scoped_to_anchor` is the existing precedent for
server-side injection into a model-originated payload and is the template
for closing this; (b) `seam:lifegraph-muninn-promotion` does not exist as
code anywhere in this repo yet (only reserved-field comments in `cypher.rs`
and `projection.rs`), so M1's "cross-plane lineage" goal is unimplementable
until that promotion path is actually built — a prerequisite for M2, not a
skipped M1 task. M2/M3 remain not started.
