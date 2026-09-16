---
title: Muninn Memory Core — Effective Create/Recall Loop, Dispersal, and the Admin Observability Plane
doc_type: proposal
domain: memory-context
status: proposed
disposition: proposed
last_updated: 2026-09-16
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
- host **disk** headroom (the ENOSPC/silent-wedge risk, DEF-078);
- **recall health** from Muninn's 0.11.0 Prometheus metrics
  (`muninndb_recall_embed_fallback_total`, `muninndb_recall_errors_total`) — a
  non-zero embed-fallback count is the **silent BM25-degradation** detector
  (recall dropped from the semantic embedder to lexical BM25). Sourcing needs a
  scrape of Muninn's `/metrics`; the report field exists and is honest
  `unavailable` until that scrape is wired (S6a-extra).

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

## Phase 2 — Philote Memory Efficiency, Sleep, and Repair (2026-09-16 audit)

Phase 1 (S1–S6a) built pieces; a live audit of all three hotels on 2026-09-16 shows
the philotes' **automatic** memory loop is still the weak link. Evidence came from
the session-event ledger (7-day retention), hotel logs (mac-jane 08-20 to 09-16),
prompt budget ledgers in `generate_text` payloads, read-only recall replays, and a
code audit of `origin/develop` (3ed106cd, the deployed truth; this branch is 72
commits behind). Slices are ordered by operator priority: **recall/remember
efficiency first**, then sleep/maintenance, then the missing-memory repair.

### Audit findings (measured)

**Recall** runs once per inbound task and on every plan continuation
(`maybe_auto_recall_turn_memory`), fans out one `/api/activate` per tokened vault,
merges, and keeps 5.

| Finding | Evidence | Code |
|---|---|---|
| `role: <name> \|` query prefix drags role-themed memories to the top | one memory in Beacon's top-3 on 165/178 recalls; A/B replay without the prefix moves the relevant memory from #7 to #2 | `memory-core/src/recall.rs` `build_query` |
| No relevance gate; always 5 items | score parsed then dropped; replayed queries: 0 strong, 24% moderate, 76% weak; a query Muninn answers with nothing still injects 5 | `rest_client.rs` merge; `recall.rs` limit 5 |
| Projection mostly wasted | 3,000-char cap truncates 97–99.5% of blocks; ~965-char fixed preamble; ~650 chars/item for ~150 chars of content, so 2–3 items survive; block re-sent on 73–89% of the turn's later model calls (~8% of every prompt) | `session/mod.rs` `project_recalled_memory` |
| LifeGraph starved by ordering | Muninn items render first and the cap cuts from the end | `session/mod.rs` layer assembly |
| Nanosecond timestamps unconverted | model sees `unix_ms=1779888583525039000`; recency tie-break is always 1.0 | `rest_client.rs` `ActivationItem` to `Engram` |
| No timeout; engine rebuilt per call | `reqwest::Client::new()`; the 45s `RecallCache` can never hit; p90 up to 543 ms, max 4–12 s, inline on the turn | `rest_client.rs`; `memory_integration.rs` `memory_engine_for` |
| Failing/empty vaults queried every turn | `user_likesjx` HTTP 401 on 207 mac-jane recalls since 08-22 (token heal must mint on the Cortex; a partial 401 never triggers heal); empty session/ariel/lyra vaults | `rest_client.rs` partial-failure path |
| Failed recalls invisible | 33/391 mac-jane recalls failed with no event, so `memory.report` hit rate reads ~100% against a real 91.6% | `memory_integration.rs` failure path; `memory_report.rs` |
| Recall never feeds back | `last_access` equals `created_at` on 166/167 Beacon and 56/56 Jane memories | no feedback/access path |

**Remembering**:

| Finding | Evidence | Code |
|---|---|---|
| Mac-hotel writes are rejected | Attend-hook `self_` writes POST to the local observer and get **421 Misdirected Request** (13×); token mint 421 (27×); no `memory.write_forward` activity fleet-wide | Attend hook hard-codes local `SelfOnly`; `is_fleet_shared_vault` excludes `self_*` |
| Writes nearly stopped | last new memory: Björk 07-11, Coach 08-03, Aria 07-28, Jane 08-05; only Beacon (vps, local Cortex) still gains (~4% of turns) | — |
| `memory.remember` rarely offered | keyword gate `looks_like_memory_write_goal`; offered in ≤2.5% of model calls, called 0× in 7 days | tool gating |
| `memory_candidate` rarely produced | 1–2% of responses | `MEMORY_CANDIDATE_POLICY` |
| Concept slug is the upsert key | `idempotent_id = "{vault}:{concept}"`; Muninn (v0.11.0+, #556) **evolves** the pinned memory on changed content, so corrections work, but two different facts under one slug clobber each other | `rest_client.rs` write paths |
| Noise in self vaults | test probes (`perplexity.note…`) recalled into unrelated turns ~150×; self-heal escalations stored as Beacon's own memories; months-old dated events rendered as current | capture paths |

**Content quality is otherwise good**: short, atomic, no pasted model responses, and
no near-duplicates among recent memories.

**Sleep/maintenance today**: `memory.hygiene` (03:00, opt-in, live on mac-jane) only
flags. The hotel dream sweep (`aiua/src/dream.rs`, opt-in, not enabled) consolidates
through REST `/api/consolidate`, a path that replicates and keeps lineage, but its
vault discovery matches no real guests and on Macs its writes would 421. Muninn
itself has no scheduled consolidation (the 6h worker is never started), no
soft-delete hard purge, and no enrich plugin on any node. **The `muninn dream` CLI
must never run on a cluster node**: it opens Pebble without the replication log, so
its archives never replicate and nodes diverge silently.

**Replication**: in both v0.11.0 and rc1 no node ever receives a snapshot on join
(`coordinator.go` builds `NewJoinHandler`/`NewJoinClient` without a DB). When an
observer falls more than `max_log_backlog` (5000) entries behind, or the Cortex
restarts while it is away, the leader prunes past it, the observer restarts from the
first retained entry, and the applier (no contiguity check) silently skips the gap.
That is the mbp-jane Aug 22–29 hole: 49 `default` memories plus every other mutation
in the window. Both observers also hold a few philote memories the Cortex never got,
written locally before they became observers: mbp 5 (`self_agent-jane` 2,
`self_agent-aria` 3) and mac-jane 2 (`self_agent-coach`), plus mac-jane's known ~179
in `default`.

### M1 — Recall query + relevance gate (quality): implement first

- Drop the `role:` prefix from `build_query`; reintroduce role only as a tag boost if
  an A/B replay shows it helps.
- For short or referential turns ("what about the second one?"), seed with the
  previous user turn. For plan continuations, reuse the turn's existing recall
  instead of querying on the boilerplate continuation brief.
- Request and parse `relevance_band`; inject `strong`/`moderate` only; allow 0–5
  items. Carry score/band onto `Engram` so projection and telemetry can see it.
- Stop querying vaults that are empty or failing: cache per-vault emptiness for the
  session, and on a partial 401 trigger token heal for that vault (minting on the
  Cortex, see M4).
- **Acceptance**: replaying the six audit queries puts the relevant memory in the top
  3 where one exists and injects nothing where none does; no single memory appears in
  more than 25% of an agent's recalls over a day.

### M2 — Projection budget (tokens)

- Slim item format: `concept — content (age, origin)`; keep ids, tags and validation
  metadata in the turn record rather than the prompt.
- Cut the fixed preamble to the rules that change behaviour (~300 chars).
- Fix the nanosecond conversion and render age as "3 days ago".
- Render the ⚠ contradiction/staleness lines once (drop the duplicate raw
  `annotations` JSON) and drop superseded items outright.
- Give LifeGraph its own lane budget instead of sharing the cut; budget the
  `MuninnEntity` overlay.
- On re-entry model calls within a turn, keep the block byte-identical and in a
  stable prefix position so provider prompt caching applies.
- **Acceptance**: under 10% of blocks truncated; recall share of prompt at most 4%;
  every injected item fully visible.

### M3 — Recall engine hygiene + honest telemetry (latency)

- One long-lived `MuninnRestEngine` per philote, so the connection pool and the
  existing recall cache are actually used.
- Recall timeout (target 1.5 s): proceed without memory and emit
  `memory_auto_recall_failed` with the reason; emit it on every failure path.
- Put per-item band/score and latency in `memory_auto_recall_completed`; make
  `memory.report` recall effectiveness count failures; populate
  `router_traces.token_count`.
- When the model cites or acts on a recalled item, record access/feedback in Muninn
  so ACT-R activation stops being inert.

### M4 — Write routing to the Cortex (memory creation): critical

- **All** philote writes (Attend hook, `memory.remember` at any scope,
  `context.capture`, MCP `memory.capture`) and vault token minting target the Cortex.
  Recommended: extend the existing `memory.write_forward` path from shared vaults to
  every vault on non-Cortex hotels, reusing aiua's authz and `apply_forwarded_write`.
  Every philote self vault already exists on the Cortex (verified 2026-09-16).
- Land the S2 remainder: a durable retry queue for forwards that fail while the
  Cortex is unreachable, surfaced in `memory.report`; never fall back to a local
  write on an observer.
- Read-after-write: Mac recall reads the local observer, so a new memory becomes
  recallable once replication lands (seconds on a healthy observer).
- **Acceptance**: zero 421s in Mac hotel logs over 24 h; new Björk/Coach/Jane memories
  appear on the Cortex and on the hotel's observer.

### M5 — Remember volume and quality

- Replace the keyword gate on `memory.remember` with a durable-fact heuristic, or
  always offer it to persona agents.
- Wire S3 deterministic operator-fact capture after M4 (so captures land), with a
  daily cap per agent.
- Make concept slugs collision-safe: include a distinguishing subject in the slug, or
  recall by concept before writing and evolve deliberately.
- Keep test probes and self-heal escalations out of persona self vaults (dedicated
  diagnostics vault); cap Attend-hook content length as the LifeGraph fork does.
- **Acceptance**: each active persona gains durable memories weekly; zero probe
  memories recalled into persona turns.

### M6 — Sleep: Cortex-side maintenance cycle

Runs **only on the Cortex hotel** (vps-jane), over REST/MCP so every mutation
replicates. Built on the existing `memory.hygiene` cron and autonomy grants, moving
each lane from `proposal_only` to `auto_with_audit` as confidence is earned:

1. Contradictions: resolve via evolve, `forget(not_true_since)` or `link supersedes`.
2. Near-duplicate clusters: `/api/consolidate` (lineage kept). Fix `dream.rs` vault
   discovery to the real `self_agent-*` / `user_*` naming and gate it to the Cortex.
3. Dated events past their date: set `valid_until` or archive, so they stop rendering
   as current.
4. Enrichment backfill: agent-driven `get_enrichment_candidates` +
   `apply_enrichment`, or `MUNINN_ENRICH_URL` on the Cortex only (never on observers).
5. Tombstones older than 30 days: `forget hard=true`.
6. Divergence check: scheduled cross-node ID+state diff per vault; alert on the Cortex
   log lines `dropping lobe left behind`, `forced_by_backlog=true` and `from_seq=0`.
7. Enable `--metrics-addr` on all three nodes and feed
   `muninndb_recall_embed_fallback_total` and `muninndb_activate_duration_seconds`
   into `memory.report`.

Never run the `muninn dream` CLI on any cluster node.

### M7 — Repair the missing memories (live-ops, operator-driven)

Supersedes the S5 reconcile note. Order matters because a restore erases
observer-only data:

1. **Enumerate observer-only memories** in every vault on both observers with the
   two-way ID diff, `default` included (mac-jane ~179; mbp to be measured).
2. **Re-write them to the Cortex** (content copy, new ULIDs, tagged
   `reconciled-from:<node>`). Duplicates are not a problem here because step 4
   replaces the observer's store.
3. Stop the observer daemon.
4. Take a fresh online checkpoint on the Cortex (`POST /api/admin/backup`), copy its
   `pebble/` to the observer, move the observer's `data/pebble` aside and install the
   checkpoint. Keep `node-identity`, `cluster.yaml`, `auth_secret`, `wal/`,
   `audit.log` and `~/.muninn/mcp.token`. Schemas match (migrations 1–6 on both
   v0.11.0 and rc1).
5. Start the observer; confirm `cluster: joined` and the Cortex's
   `starting replication stream … from_seq` line; re-run the ID+state diff.
6. Rollback: stop the daemon and move the old `pebble` back.

Run steps 3–5 back to back, so fewer than 5000 log entries land between checkpoint
and rejoin. Never wipe-and-rejoin (no snapshot path exists, so the observer would end
up far emptier) and never `vault import` (observers reject it, and it cannot merge
into a live vault).

**Prevent recurrence**: raise `max_log_backlog` to cover the longest laptop offline
window (size it from the `log_seq` rate); upstream to muninndb an applier contiguity
guard (fail loud with a `needs_resync` marker), a per-vault digest endpoint and real
snapshot wiring; build rc1 for linux so the self-knowledge surface works on the node
every write goes through.

### Phase 2 sequence

**M1 → M2 → M3** (one PR rebased on develop; test-green, then watched-live on one
persona per hotel) → **M4** (with the S2 queue) → **M5** → **M6** → **M7** (operator
window). M7's prevention items and the linux rc1 build can run in parallel.

**Security note from the audit**: mac-jane's `config:muninn` graph node holds Muninn
admin credentials in plaintext with a default-looking password. Rotate them and move
them to the vault store; tracked separately from this proposal.

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

All five **in-repo** slices are implemented and test-green on `codex/muninn-memory-core`
(PR #466): S1 (fleet-knowledge recall scope), S2 (write-routing completeness +
loud strand), S6a (admin `memory.report` tool on the honest-sourcing assembler),
S4 (promotion criterion + posture gate), and S3 (deterministic capture
classifier). S3 and S4 land their judgment-heavy cores test-green; their
store-mutating wiring (S3 attend-hook capture, S4 sweep fetch+write) is
deliberately held until it can be **live-validated**, since those paths write to
the store this proposal is hardening.

The remaining two slices are **not in-repo code**: **S6b** needs a new
replication-status API in the `muninndb` Go project (cross-repo + deploy), and
**S5** is live-ops (pin binaries, repair the observer apply layer, merge the two
muninndb fix branches, fleet deploy, reconcile the 179 mac-only memories first)
that restarts the live memory daemon and must be driven with the operator.

See [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

> Graph-intelligence was unavailable during authoring; this proposal is
> file-backed and its graph node/decision are deferred until the graph server
> is reachable from the main checkout (graph-only proposals are wiped by
> `clear_scanned_doc_nodes`; doc-backed ones index from the main checkout).
