---
title: Autopoiesis — Closing the Loops of a Self-Building World
doc_type: proposal
domain: operator-control-plane
status: active
disposition: accepted-current-slice
last_updated: 2026-07-11
tags:
- autopoiesis
- self-building
- gated-autonomy
- feedback-loops
- north-star
related_docs:
- LIFE_GRAPH_OS_PROPOSAL.md
- AGENT_RESOURCE_MODEL_PROPOSAL.md
- ARCH_RULES.md
- SUBSTRATE_HARDENING_PROPOSAL.md
- MEMORY_TRANSPARENCY_PROPOSAL.md
task_refs:
- docs/task.md
---

# Autopoiesis — Closing the Loops of a Self-Building World

> **Operator intent (2026-07-07, verbatim):** "I really want this philotic system
> to be a self-building world."

## Goal

Philotic already contains every organ of a self-building system — a repair
dispatcher, a retrieval feedback engine, an attention steward, a project
intelligence graph with a full proposal→seam→slice process, and agents capable
of authoring and shipping code. What it lacks is closure: **every loop in the
system currently terminates in prose or in a human.**

- `life.recall.feedback` generates a `SystemPatch` *description* — and stops.
- The heal queue classifies failures and restarts guests — but a *recurring*
  failure pattern never becomes a filed fix.
- The attention steward evaluates signals — observe-only, forever, because its
  activation gate exists only as proposal prose.
- The SVE process holds 200+ seams — every one waiting for a human to notice it.

The goal of this epic: **connect each loop's last mile, under earned,
per-lane autonomy** — so the system converges toward noticing its own gaps,
proposing its own work, and executing the safe subset without asking.

## Core Recommendation

Generalize the pattern the codebase already trusts. Three precedents prove it:

1. **`PatchRisk` gating** (`data-memorygraphrag`): Low → auto-apply with audit,
   Medium → confirm-first, High → proposal-only.
2. **Respawn budgets** (`guest_manager`): autonomy with a flap ceiling and a
   visible escalation when the budget exhausts.
3. **Tool pre-approval promotion** (`philote` approval policy): a success
   streak earns standing approval; a failure resets it.

Fuse these into one first-class concept — the **AutonomyGrant** — attached to
each self-building lane:

```
AutonomyGrant {
  lane: "graph.bridge_edges" | "fleet.heal_slices" | "work.file_proposals" | ...,
  posture: ProposalOnly | ConfirmFirst | AutoWithAudit,
  budget: { max_actions_per_day, max_consecutive_failures },
  earned: { promotions require N confirmed-good outcomes; any operator
            reversal demotes one posture level },
  audit: every autonomous action writes a decision record (intel graph or
         LifeGraph Signal) with what/why/evidence — reviewable, reversible,
  kill_switch: per-lane env override, always
}
```

The operator approves **postures, not patches**. Trust is earned per-lane,
never global, and demotion is automatic on reversal.

## The Loops, and Their Last Miles

| # | Loop | Today ends at | Closed means | Lane |
|---|------|---------------|--------------|------|
| 1 | Retrieval feedback → graph structure | prose `SystemPatch` node | Low-risk patches (bridge `RELATES_TO` edges between the feedback's connected candidates) actually MERGE into Memgraph, audited | `graph.bridge_edges` |
| 2 | Heal queue → fleet fixes | restart / escalate | A recurring pattern (same `pattern_tag` ≥ N times / window) auto-files an intel-graph proposal with journal evidence attached; coding agents pick it up | `fleet.heal_slices` |
| 3 | Observation → work | human reads dashboards | Aria's architect charter: sweep heal queue, error logs, defect ledger, seam staleness on a cadence; author scored proposals into the intel graph | `work.file_proposals` |
| 4 | Steward signals → operator nudges | observe-only policy | The "5 confirmed SIL entries" gate implemented in code: confirmed entries unlock bounded active check-ins (max/day budget) | `steward.active_checkins` |
| 5 | Proposal → implementation | operator triggers a session | Scheduled agent sessions claim the top-scored open proposal within granted lanes and execute the SVE loop end-to-end | `work.execute_slices` |
| 6 | Repeated success → shared skill | plan-eval history sits unread | A tool-pattern that keeps succeeding (same tool sequence across ≥ 3 completed plans, per the plan-eval records from #162) is distilled into a named `abstract_skill` and proposed through the authz-gated `skill.register` path (#143); operator approval registers it, and the skill becomes projectable to *other* philotes' toolset profiles — knowledge propagating through the team | `skills.register_learned` |
| 7 | Delegation outcomes → team shape | whisper outcomes evaporate | Paracrine whisper outcomes accumulate as delegation memory, so orchestrators learn *who* to whisper to; stewards propose amendments to their own charters, applied by the operator through ConfigureRole | `team.evolve` |
| 8 | Merged PR → running fleet | operator deploys by hand | Canary deploy: new binary rolls to ONE hotel, burn-in window watching heal queue / pattern tags / doctor, then promote to the fleet or auto-rollback with the evidence filed as a proposal (the loop-2 filing pattern, reused) | `fleet.canary_deploy` |

Loop 5 is deliberately last among the execution loops: it is the full
autopoietic cycle, and it should only run after lanes 1–4 have produced the
track record that earns it. Loops 6 and 7 are the team dimension of the same
idea — loop 6 builds the *skills* the team shares, loop 7 builds the *team*
itself. Neither executes anything; both end in proposals a human approves.

Loop 8 is the loop the original seven missed: without it, every "closed" loop
still terminates in the operator — just later, at the deploy step. It only
runs above a hardened substrate (see
[SUBSTRATE_HARDENING_PROPOSAL.md](SUBSTRATE_HARDENING_PROPOSAL.md)) because
rollback and burn-in judgments are only trustworthy when supervision, the
heal circuit, and machine-readable verification are themselves reliable.

## Slices

| Slice | Content | Size | Verify |
|---|---|---|---|
| A1 `autonomy-grant-core` | `AutonomyGrant` record (graph-backed, per-lane), posture state machine with earn/demote transitions, audit-record writer, env kill switches. Pure library + storage; no lane wired yet. | M | test-green |
| A2 `feedback-to-action` | Wire `recall_feedback_patch_proposal`'s SafeAutoUpdate cases through lane `graph.bridge_edges`: disconnected/missing feedback with clear candidates writes the bridge edge (living-cycle vocabulary, idempotent MERGE) instead of stopping at prose. ConfirmFirst posture by default. | M | test-green + live feedback round-trip |
| A3 `heal-pattern-filing` | heal-dispatcher counts `pattern_tag` recurrences; threshold breach files an intel-graph proposal (REST :8900) with the last N raw log lines as evidence. ProposalOnly posture — filing IS the action. | S–M | smoke-green (induced recurring failure files exactly one proposal) |
| A4 `aria-architect-charter` | Role manifest + daily cron for Aria: sweep heal queue / DEFECTS / seam staleness, author or update scored proposals, morning dev-brief to operator. Config-shaped, mirrors Beacon's chief-of-staff charter. **Still unstarted as code/config in this repo** as of Memory Transparency M3 (2026-07-11) — a `memory.delta_digest` philote tool now exists (`crates/philote/src/catalog.rs`/`memory_integration.rs`) specifically so this charter's eventual daily-brief prompt can call it before composing the morning brief; whoever builds A4 should wire that call in rather than re-deriving a memory-delta summary from scratch. | S | watched-live (first authored proposal reviewed by operator) |
| A5 `steward-activation-gate` | Implement the SIL confirmation counter and bounded active check-ins behind lane `steward.active_checkins`, ConfirmFirst until 5 confirmed entries, then AutoWithAudit with a max-per-day budget. | M | test-green + live confirmation cycle |
| A6 `scheduled-slice-executor` | A scheduled session (cloud or cron) claims the top open proposal in granted lanes, runs the SVE loop (worktree → slice → verify → PR), reports to operator. Starts ProposalOnly (drafts the plan, does not merge). | L | watched-live, operator reviews first N runs |
| A7 `skills.register_learned` | When plan-eval-repeat (#162) records the same tool sequence succeeding across ≥ 3 completed plans, distill it into a named `abstract_skill` and propose it via the authz-gated `skill.register` path (#143). ProposalOnly — operator approval registers the skill, which then becomes projectable to other philotes' toolset profiles. Earned promotion: after 5 approved skills, ConfirmFirst. | M | test-green + first operator-approved skill projected onto a second philote |
| A8a `team.evolve` — delegation memory | Record paracrine whisper outcomes per (orchestrator, specialist, task-class) as graph records, so orchestrators learn who to whisper to. Pure observation — no lane needed. | S–M | test-green + delegation records visible after live whispers |
| A8b `team.evolve` — charter evolution | A steward may propose amendments to her *own* charter, `life.patch.propose`-style. ProposalOnly forever by default; the operator applies accepted amendments via the now-safe ConfigureRole path (#179). | M | test-green + first charter amendment proposal reviewed by operator |
| A9 `trust-ledger` | Make posture promotion **arithmetic instead of vibes**. (a) Every AutonomyGrant audit record gains an `outcome` field — `confirmed_good` / `reversed` / `neutral` — stamped by operator confirmation, reversal detection, or timeout-to-neutral. (b) `phil autonomy status` (+ doctor section) reports per-lane: actions/day vs budget, consecutive failures, confirmed-good streak, and computed promotion eligibility straight from the earn/demote rules. The A6 gate ("≥ 2 weeks clean audit") becomes a query result. Outcome stamps are also the training signal for A7 and the model flywheel. **Outcome-stamping follow-up (2026-07-21):** the `AuditOutcome`/`RecordAutonomyOutcome`/`promotion_eligible`/`phil autonomy status` core shipped but nothing stamped an outcome, so streaks stayed 0 fleet-wide — closed by (1) a daily `internal:autonomy_outcome_sweep` cron job (`aiua::autonomy_sweep`) that stamps audits `Pending` past `PHILOTIC_AUTONOMY_NEUTRAL_AFTER_DAYS` (default 7d) `Neutral`, gated per-hotel by a job-id match (not a local-enabled flag, since the sweep has no opt-in — see module docs for the mesh-trap this closes) so `CronJobSync` replication never lets one hotel sweep a peer's records; (2) `phil autonomy pending` (lists the raw `Pending` backlog) and `phil autonomy stamp <id> confirmed_good\|reversed\|neutral` (operator-driven `RecordAutonomyOutcome`); (3) an unresolved, throttled `autonomy_outcome_pending` heal-queue breadcrumb pushed alongside each fresh filing (A3 heal-pattern-filing and M4 memory.hygiene sites) — visibility only, next seam is a live Telegram push with inline buttons. | M | test-green + doctor/status output shows a real lane's eligibility |
| A10 `fleet.canary_deploy` | Loop 8's lane. A deploy executor rolls a merged, verified binary to one designated canary hotel, watches heal queue / pattern tags / doctor for a burn-in window, then promotes fleet-wide or auto-rolls-back (backup binary path named in the audit record) and files the evidence as a proposal. ConfirmFirst at every step initially: proposes the canary, proposes the promotion. | L | watched-live (first canary cycle operator-reviewed end to end, including one induced rollback) |

The A8c frontier — a steward *executing* team changes through `SpawnSubagent`
(the wire contract that today returns an explicit `SUBAGENT_NOT_IMPLEMENTED`
rejection) — is explicitly deferred until A6 ships and has a track record.

Dependency: A1 → {A2, A3, A5}; A4 independent (config); A6 after A9 reports
eligibility (the "≥ 2 weeks of clean audit records" gate, now mechanical).
A7 depends on A1 plus the plan-eval records from #162; A8a is independent
(pure observation); A8b depends on the ConfigureRole ladder fix (#179);
A8c waits on A6. A9 depends only on A1's audit records. A10 depends on A9
plus SUBSTRATE_HARDENING slices S1–S3 (supervision invariant, heal-the-healer,
verification-as-data) being live.

**Prerequisite:** the LifeGraph epic's in-flight slices (retrieval lane 3–4,
charter, hygiene, auto-capture) land first — a self-building world needs its
world-model working. A2 specifically builds on the retrieval lane's Slice 4
(Muninn provenance) landing.

**Substrate prerequisite (added 2026-07-11):** autonomy compounds whatever it
sits on. No lane is promoted past ConfirmFirst — and A6/A10 do not run at
all — until [SUBSTRATE_HARDENING_PROPOSAL.md](SUBSTRATE_HARDENING_PROPOSAL.md)
S1–S3 are live: every hotel under a real supervisor, the heal circuit able to
heal its own dispatcher, and verification recorded as machine-readable data.
The memory dimension of the same discipline lives in
[MEMORY_TRANSPARENCY_PROPOSAL.md](MEMORY_TRANSPARENCY_PROPOSAL.md) — memory
writes are actions and carry the same auditable/reversible/budgeted contract.

## Autonomy Contract (standing rules)

1. Every autonomous action is **auditable** (decision record with evidence),
   **reversible** (soft-retire / revert path named in the audit record), and
   **budgeted** (daily caps; consecutive-failure ceilings).
2. Postures only promote on **operator-confirmed outcomes**, and demote
   automatically on any operator reversal. No lane ever starts above
   ConfirmFirst.
3. Per-lane kill switches (`PHILOTIC_AUTONOMY_DISABLE_<LANE>`) override
   everything, always.
4. The intel graph is the ledger of self-directed work; the LifeGraph is the
   ledger of operator-world effects. An action that touches neither did not
   happen — and is therefore forbidden.

## Disposition

`accepted-current-slice` — A1–A5 are implemented as of 2026-07-08:

- **A1 `autonomy-grant-core`** — PR #156
- **A2 `feedback-to-action`** — PR #163
- **A3 `heal-pattern-filing`** — PR #161
- **A4 `aria-architect-charter`** — staged charters applied: Beacon (vps-jane),
  Aria (mbp-jane); Coach (mac-jane) in flight
- **A5 `steward-activation-gate`** — PR #165

**A6 `scheduled-slice-executor`** is awaiting trust accumulation — the ≥ 2
weeks of clean audit records from A2–A4 that its dependency line demands.
A9 turns that gate from a judgment call into a query.

Current slices: **A7 `skills.register_learned`** (skill-building), **A8**
(team-building, sub-slices A8a/A8b), and **A9 `trust-ledger`** (added
2026-07-11 — the mechanical promotion ledger that unblocks A6).
**A10 `fleet.canary_deploy`** is specced but blocked on A9 +
SUBSTRATE_HARDENING S1–S3.

**A9 outcome-stamping follow-up** (2026-07-21, `codex/autonomy-outcome-stamping`):
the `AuditOutcome`/`RecordAutonomyOutcome`/`promotion_eligible`/status-report
core landed but nothing ever moved a `Pending` audit off dead center, so no
lane could accumulate a confirmed-good streak. Closed the loop: a daily
timeout-to-Neutral sweep (`aiua::autonomy_sweep`, mesh-trap-safe via a
per-hotel job-id gate), `phil autonomy pending`/`stamp` operator surfaces, and
an unresolved pending-outcome heal-queue breadcrumb at the two existing
filing sites (A3, M4). test-green (targeted `cargo test -p aiua`/
`-p ansible-mesh-core`; the daily cron itself was not watched live — see PR
for the honest verification level). A6's "≥ 2 weeks clean audit" gate is now
mechanically answerable once real traffic accumulates through the stamp
surfaces or the timeout sweep.
