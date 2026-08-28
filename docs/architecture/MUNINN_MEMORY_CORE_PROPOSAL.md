---
title: Muninn Memory Core — Effective Create/Recall Loop, Dispersal, and the Admin Observability Plane
doc_type: proposal
domain: memory-context
status: proposed
disposition: proposed
last_updated: 2026-08-28
tags:
  - muninn
  - memory
  - recall
  - replication
  - cortex
  - observability
  - admin-plane
  - hardening
related_docs:
  - KNOWLEDGE_ARCHITECTURE_PROPOSAL.md
  - MEMORY_CULTIVATION_TRUE_UP_PROPOSAL.md
  - MEMORY_LAYERING_AND_WORK_PRODUCT_SPLIT_PROPOSAL.md
  - MEMORY_TRANSPARENCY_PROPOSAL.md
  - MUNINN_MEMORY_PROTOCOL_PROPOSAL.md
  - MUNINN_VPS_REHARDEN_PROPOSAL.md
  - MUNINN_CLUSTER_EVALUATION_CHECKLIST.md
  - CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: muninn-memory-core
implements:
  - cross-agent-knowledge-architecture
  - memory-cultivation-and-true-up
active_seams:
  - fleet-knowledge-recall-scope
  - memory-write-routing-completeness
  - deterministic-memory-capture
  - memory-cultivation-mutations
  - muninn-replication-repair
  - muninn-admin-observability-plane
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
---

# Muninn Memory Core — Effective Create/Recall Loop, Dispersal, and the Admin Observability Plane

## Goal

Make Muninn **the** hardened core memory store for the fleet, so that:

1. philotes reliably **create** appropriate memories from their turns (not only when a model chooses to),
2. philotes reliably **use** memories — including the fleet's shared knowledge — in every relevant turn,
3. writes reliably **reach the Cortex** and **disperse** to the read replicas,
4. an **admin philote** has full visibility into and management control over Muninn, backed by real reporting data — never confabulated health.

This proposal consolidates threads that are each individually proposed
(cross-agent knowledge, cultivation/true-up, memory layering, cluster reharden)
but that collectively are **not delivering an effective memory loop today**. It
names the concrete implemented-vs-designed gaps found in the 2026-08-27/28
audit and defines the slices that close each one.

## Current Reality (the audit)

Diagnosis from a live trace of the create/recall/disperse path
(`crates/philote/src/memory_integration.rs`, `crates/aiua/src/memory.rs`,
`crates/memory-core/src/rest_client.rs`) and the live cluster:

- **USE / recall is well-engineered and wired.** `maybe_auto_recall_turn_memory`
  (`memory_integration.rs:1674`, called `runtime.rs:3214`) runs per user turn,
  builds a rich `RecallContext`, gates on `evaluate_recall.should_recall`,
  self-heals a token-401, and `project_recalled_memory` (`session/mod.rs:3335`,
  rendered at `:2386`) injects a provenance-tagged, precedence-ruled
  `[Recalled memory]` block into the model prompt. This half is good.
- **GAP 1 — the shared knowledge vault is unreachable by philotes.**
  `MemoryScope` (`memory-core/src/types.rs:16`) has only `SelfOnly`,
  `SharedUser`, `Session`, `CrossScope`. `VaultResolver::resolve`
  (`rest_client.rs:148`) maps them to `self_<agent>` / `user_<user>` /
  `session_<id>`. **No scope addresses the `default` vault** — the 800+-memory
  Cortex knowledge base — so philotes structurally cannot recall fleet
  knowledge. Claude writes `default`; philotes read `self_*`. Two disjoint
  worlds.
- **GAP 2 — create is model-gated and locally scoped.** Auto-capture fires only
  when the model emits a `memory_candidate` field, and the attend hook
  (`turn_loop.rs:3614`) is hardcoded `SelfOnly` → a local `self_<agent>` vault
  that is never forwarded. Emission is at the model's whim, with no independent
  extraction of durable operator facts.
- **GAP 3 — write routing is incomplete and fails silently.** Only
  `memory.remember` on fleet-shared vaults forwards to the Cortex;
  `apply_forwarded_write` (`aiua/src/memory.rs:109`) **hard-rejects any op other
  than `remember`**, and `evolve`/`forget`/`remember_batch`
  (`rest_client.rs:774-903`) always hit the local node. Forward failure
  **silently falls back to a local write** (`memory_integration.rs:2258`), so a
  shared memory can strand on a replica with only a log warning.
- **GAP 4 — cultivation is report-only.** `memory.cultivate`/`true_up` and the
  `memory.hygiene` cron (`aiua/src/memory_hygiene.rs`) are fetch/report passes
  (`mutation_performed: false`); the mutating cultivation the
  MEMORY_CULTIVATION_TRUE_UP proposal describes is unimplemented, so nothing
  consolidates, de-duplicates, promotes, or archives.
- **GAP 5 — Cortex→observer replication is broken, and the fleet has no
  visibility into it.** A live write on the Cortex did not reach the mac
  observer; the observer runs a `dev` binary (skew vs Cortex `v0.11.0`) and
  flaps on the `:8490` replication port. Divergence: observer 926 vs Cortex 822
  in `default`. Two of our own replication-fix branches
  (`fix/observer-cache-invalidation` #869, `fix/replication-log-retention`) are
  unmerged into muninndb `develop`.
- **GAP 6 — no admin observability/management plane.** `muninn_status` returns
  only `{vault, total_memories, health, enrichment_mode}` — no cluster
  topology, replication lag, peer state, per-agent write rates, or recall
  effectiveness. There is no admin-role surface to *manage* Muninn
  (reconcile, resync, prune, cultivate, promote). The admin philote is blind.

Net effect: philotes run on **thin, local, self-only memory**; the shared
knowledge base never flows into their turns; writes do not disperse; and no one
can see or manage the store's health.

## Core Recommendation

Close the loop in six slices, sequenced **S6a → S1 → S2 → S3/S4 → S6b → S5**,
each a smallest-honest-slice with its own branch/PR. The proposal describes all
six; implementation lands one at a time.

Guiding constraints:

- **The admin philote must never report health it cannot see** (see
  `beacon-confabulates-cron-claims` precedent). Reporting is split into what is
  sourceable today vs. what needs a new muninndb API.
- **Recall must inform, not drown.** The fleet-knowledge scope reads a *curated*
  vault (trust/importance-filtered), never the raw `default` engineering log.
- **Every mutating admin verb declares its own gate.** Do not inherit the skill
  admin plane's pattern wholesale — its own audit found `assign`/`set_state`
  were not human-gated while `register` was.

## Slices

### S6a — Muninn Admin Observability (reporting, read-only) — implement first

An admin-role-gated read surface aggregating the data that **exists today**:

- per-vault counts + health on **each** node (`muninn_status` against the local
  observer and the Cortex admin endpoint via `muninn_provision`'s
  `MuninnAdminCredential`, `aiua/src/muninn_provision.rs:223`);
- **divergence** between observer and Cortex (`observer_ids − cortex_ids`) via
  the `muninn_session` write-log sweep method proven in the audit;
- **recall effectiveness** — `memory_auto_recall_completed` vs `_skipped` rates
  — from the aiua session-event ledger (these are philote turn events, not
  Muninn tools);
- **write activity** — per-agent/per-vault `memory.remember` forward counts from
  the same ledger;
- **contradictions / stale candidates** (`muninn_contradictions`, the hygiene
  sweep) and **soft-deleted** counts (`muninn_list_deleted`);
- host **disk** headroom (the ENOSPC/silent-wedge risk, DEF-078).

Surface: a `memory.report` philote tool (admin-role gated, read-only) returning
a structured report, and — stretch — a Muninn panel in philotic-web (the
management plane). No mutation in this slice → simplest authz. Every field must
name its source and its freshness; fields that cannot be sourced are reported as
`unavailable`, never guessed.

### S6b — Muninn Admin Observability (fields requiring a muninndb API)

Replication lag, peer/streamer state, replication-log backlog depth, and
per-node apply status are **not** exposed by any current Muninn MCP tool. This
slice adds the muninndb-side API (upstream fork) and consumes it in the report.
Blocked on muninndb work; kept distinct so S6a does not ship half-fabricated.

### S1 — Fleet-knowledge recall scope

Add a `SharedFleet` `MemoryScope` variant resolving to a curated
`fleet_knowledge` vault, and include it (trust/importance-filtered) in
`default_turn_recall_scope`. **Ripple to check:** `VaultResolver::resolve` /
`resolve_primary`, and critically `is_fleet_shared_vault`
(`rest_client.rs:55-57`, currently `default` or `user_*`) — the predicate that
decides Cortex forwarding — must include `fleet_knowledge` or its writes strand
locally.

**Dependency:** `fleet_knowledge` is **empty until S4's promotion fills it**, so
S1 alone recalls nothing. S1 must either seed the vault explicitly (define what
goes in and who decides) or ship paired with S4. Sequenced after S6a because the
report tells us whether the scope is actually being populated/recalled.

### S2 — Write-routing completeness + fail-loud

- Op-dispatch `apply_forwarded_write` (`aiua/src/memory.rs:106`) to handle
  `remember` | `evolve` | `forget` | `remember_batch` instead of rejecting
  non-`remember`.
- Route those verbs through `forward_shared_memory_write` on the philote side.
- Turn the silent local-write fallback (`memory_integration.rs:2258`) into a
  **loud** failure surfaced to the tool result, plus a durable reconcile queue
  so a stranded shared write is retried against the Cortex rather than lost.

### S3 — Less model-dependent capture

Add a deterministic operator-fact/preference classifier (mirroring
`life_capture::classify_lived_fact`, `life_capture.rs:255`) that proposes Muninn
candidates for durable operator facts/preferences, so capture is not purely at
the model's whim. Keep model-`memory_candidate` as the primary path; the
classifier is a floor. Verify live emission rate via the S6a report before and
after.

### S4 — Cultivation mutations + promotion

Implement the designed-but-unbuilt mutating cultivation
(consolidate / evolve / de-duplicate / archive) behind the existing
`memory.hygiene` cron and autonomy grants (`proposal_only` → autonomous by
confidence). Add a **promotion** path that lifts durable, high-value
`self_`/`session_` memories into `fleet_knowledge` (which forwards + is
recalled), closing dispersal for the memories that matter. Promotion is gated
and classifier-filtered (privacy: `self_` memories may carry sensitive
per-agent context — promotion is opt-in by policy, not automatic for all).

### S5 — Replication repair + version pin + branch merges (infra)

- Merge `fix/observer-cache-invalidation` (#869) and
  `fix/replication-log-retention` into muninndb `develop`.
- Pin every node's Muninn binary to the released version (kill the mac `dev`
  skew) and roll via the Homebrew tap.
- Repair/verify the observer apply layer and the `:8490` connectivity; confirm a
  Cortex write lands on the observer.
- **Pre-req:** reconcile the 179 historical mac-only memories up to the Cortex
  before any observer reseed (data-loss guard).

## Admin plane authz (S6a / S6b / S4 verbs)

Every verb declares its own gate; destructive verbs are never inherited as
"admin-implies-allowed".

| Verb | Slice | Kind | Gate |
|---|---|---|---|
| `memory.report` / cluster read | S6a/S6b | read-only | admin-role, audited read |
| `memory.reconcile` (push diverged up) | S6b | additive | admin-role + explicit confirm; idempotent |
| `memory.resync` (restart replication) | S6b | disruptive | admin-role + operator-approved ceremony |
| `memory.prune` (drop replication log / rows) | S6b | **destructive** | admin-role + operator-approved + dry-run first |
| `memory.cultivate --apply` | S4 | mutating | autonomy-grant gated (`proposal_only` default) |
| `memory.promote` (self → fleet) | S4 | mutating + privacy | classifier-filtered + policy opt-in + audit |

## Risks and Non-Goals

- **Recall noise.** Dumping `default` into persona prompts would flood them; the
  curated `fleet_knowledge` vault + trust/importance filter is the guard.
- **Promotion privacy.** `self_` memories can be sensitive; promotion is
  policy-gated, not blanket.
- **Scope ripple.** A new `MemoryScope` touches every `resolve`/`resolve_primary`
  caller and `is_fleet_shared_vault`; S1 must update all of them.
- **Confabulated health.** S6a reports only sourceable fields; the rest are
  `unavailable`. This is a hard rule, not a preference.
- **Not** a rewrite of Muninn's internals, a new memory engine, or a change to
  the LifeGraph plane — those stay as they are.

## Current Slice

`S6a — Muninn Admin Observability (reporting, read-only)`. See
[docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

> Graph-intelligence was unavailable during authoring; this proposal is
> file-backed and its graph node/decision are deferred until the graph server
> is reachable from the main checkout (graph-only proposals are wiped by
> `clear_scanned_doc_nodes`; doc-backed ones index from the main checkout).
