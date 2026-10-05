---
title: Watch-Live Burn-Down — Prove or Retire Merged-but-Unproven Work
doc_type: proposal
domain: workflow-docs
status: proposed
last_updated: 2026-10-05
tags:
- verification
- watched-live
- sver
- proposals
- defects
- backlog
related_docs:
- ../process/WORKFLOW.md
- VERIFICATION_LADDER_PROPOSAL.md
- PROCEDURAL_GRAPHS_PROPOSAL.md
- RELOCATION_CEREMONY_PROPOSAL.md
- MEMORY_TRANSPARENCY_PROPOSAL.md
- AUTOPOIESIS_PROPOSAL.md
- DECISIONS_MODEL_PROPOSAL.md
proposal_id: watch-live-burndown
implements: []
implemented_by: []
active_seams:
- watch-live-inventory
- defects-status-vocabulary
- proposal-status-reconciliation
- procedure-run-proof
- memory-hygiene-proof
- relocation-r6-proof
- doctor-watch-live-readiness
---

# Watch-Live Burn-Down — Prove or Retire Merged-but-Unproven Work

Origin: Philotic Stack Atlas (2026-09-30), next seam #2. Facts re-checked
against `origin/develop` @ `f170cb88` on 2026-10-05.

## Problem

The Runtime Truth Rule (`docs/process/WORKFLOW.md:155-168`) says watched-live
claims need proof that the installed runtime changed:

- the binary path changed;
- the supervisor restarted;
- the process is running the new binary;
- the observed behavior came from that process.

A large amount of merged work has never met that bar:

- **About 19 task-list items** are merged and waiting on a live proof.
  Examples:
  - procedural graphs P1–P4, with **zero `procedure_run` rows** on any hotel;
  - the memory-hygiene nightly, which no deploy config enables;
  - relocation R1–R7;
  - model-catalog freshness;
  - heal H0/H1;
  - surface buttons;
  - the MCP endpoint steward and the integration steward;
  - Lyra.
- **18 DEFECTS rows** use compound statuses like "fixed (pending live
  confirmation)" or "fixed (deploy pending)". The legend (`DEFECTS.md:12`)
  allows only `open` and `fixed`. The oldest are about 5 weeks old
  (DEF-088…099).
- **Proposal labels are wrong in both directions.** At least 20 docs have a
  `status` that disagrees with their own `disposition` or with merged code:
  - MCP_MEMBRANE_HARDENING says `proposed`, yet H1–H5 landed;
  - MUNINN_MEMORY_CORE says `proposed`, yet M1–M6 merged;
  - DECISIONS_MODEL says "nothing is wired" while D2 is live;
  - RELOCATION_CEREMONY says "R6 not started" while R2–R7 merged.
  - There is also spelling drift (`in_progress` / `in-progress` /
    "accepted for current slice"), and LIFE_GRAPH_ACTIVE has no frontmatter.
- **Nothing measures readiness.** `phil doctor` can't say whether a hotel
  runs a current binary, whether opt-in flags are set, or whether autonomy and
  procedure state is moving.
- **Two "watch-live" items aren't built at all:** Self-Improvement L6 and
  Reflexive LG R2–R5 / Graph Doors G2. They inflate the backlog.

## Goal

1. One inventory table with an owner and next action for every unproven item.
2. Each item ends **proven** (watched-live with evidence in the Intel Graph)
   or **retired** (feature flagged off, or deleted with a decision record)
   within four weeks.
3. Status vocabularies are enforced by lint, so labels can't drift again.
4. New `proposed` documents are frozen until the implemented and verified
   counts catch up. Exception: documents the operator explicitly requests.

### Non-goals

- Building unbuilt features. They leave the watch list and return to
  `proposed`.
- Forcing behavior that only natural traffic should produce, unless the
  operator approves a staged demo. See decision 2.

## Slices

### W0 — Inventory and vocabulary (S)

1. Add `docs/process/WATCH_LIVE_BACKLOG.md`. Each row has: item, proposal,
   proof needed, hotel, blocker, owner, next action, due date. Seed it from the
   table below. It is the single place to look.
2. **Tighten the DEFECTS status vocabulary.** Allowed values: `open`,
   `partial`, `fixed (deploy pending)`, `fixed (live pending)`,
   `fixed (verified)`, `resolved`, `wontfix`.
   - Update the legend at `DEFECTS.md:12`.
   - Normalize the 18 compound rows.
3. **Proposal status vocabulary.** Use exactly the `DOCUMENTATION_LIFECYCLE.md`
   states: `proposed`, `accepted`, `accepted-current-slice`, `in-progress`,
   `implemented`, `verified`, `architecture`, `superseded`, `deferred`,
   `archived`.
4. **Lint.** Extend `scripts/docs-metadata-check.py` (today it checks required
   keys only) to:
   - reject unknown `status` values;
   - flag `status` vs `disposition` mismatches;
   - flag DEFECTS rows outside the vocabulary.
   Run it in `pr-check.yml` as a non-blocking warning for one week, then make
   it blocking.

### W1 — Close the cheap ones by evidence (S, one session)

Close items where evidence already exists or a deploy plus a log grep proves
them:

| Item | Evidence or action |
|---|---|
| DEF-088…099 (Telegram/plan batch, 08-27/28) | Journal greps for their log signatures, e.g. DEF-088 draft creation and DEF-095 `plan_stopped` "superseded". Many deploys since. Mark `fixed (verified)` or reopen. |
| DEF-201…206, 208, 212 | Deploy current develop to all three hotels. DEF-212 additionally needs the live revocation UAT (`mcp-client-uat.sh agent-frontdoor`), which the 10-01 handoff reports passing. Record it. |
| Model-catalog freshness (DEF-202/203/204) | After the deploy: a `model_profile:*:vps-jane-aiua-01` row appears on mac and mbp, and mac's catalog refreshes within 6 h. Delete the two legacy rows. |
| Heal H0/H1 (DEF-205/206/208) | The stale sweep moves rows to `stale`, and fallback tags appear in the journal (flag already on in `jane-vps.yml`). |
| Cortex viewer iPhone | The operator confirmed on 10-01 (`task.md:60`). Fix the proposal text at `CORTEX_VIEWER_PROPOSAL.md:44,122`. |
| Peer delegate inbound, gardener slice 7, 11:00 brief, T1 catalog file | Passive. Check after the next occurrence and record it. |
| Self-Improvement L6, Reflexive R2–R5, Graph Doors G2 | **Unbuilt.** Move them off the watch list and back to `proposed`. |

### W2 — Proposal status reconciliation (S–M)

For each mismatched doc:
1. Edit the frontmatter. The doc is the durable truth: DEF-072 says MCP-only
   graph edits don't survive a rescan.
2. Add or refresh a `## Disposition` section with per-slice SVER levels.
3. Rescan the graph and confirm with `phil graph proposals`.

Known list:
- MCP_MEMBRANE_HARDENING
- DESKTOP_MEMBRANE
- DECISIONS_MODEL
- RELOCATION_CEREMONY
- MUNINN_MEMORY_CORE
- FLEET_SUPERVISION
- GRAPH_LAYER_UNIFICATION
- MCP_MEMBRANE_GATEWAY, WHISPER_PROTOCOL, MLX_MODEL_CONTROLLER
- DISCORD_MEMBRANE, PHILOTIC_WEB, ROUTED_OPERATOR_CHAT, CONTEXT_GRAPH_RUNNER,
  DREAM_ENGINE_COORDINATION, INTERACTIVE_ONBOARDING, TRANSCRIPTION_FLYWHEEL,
  PHILOTIC_DEPLOYMENT
- REFLEXIVE_LIFE_GRAPH
- SUBSTRATE_HARDENING
- COGNITIVE_LOOP, DISTRIBUTED_CRON
- SCRIPTED_TURN_LOOP (`draft`)
- LIFE_GRAPH_ACTIVE (add frontmatter)
- REFLEX_ENGINE_E2E_VALIDATION (`planning`)

Use the `proposal-maintainer` skill. Batch into about 3 PRs, by domain.

### W3 — Procedural graphs proof (M)

**Why there are zero rows.** The trigger chain is narrow.
`plan_eval::reports_an_outcome` (`plan_eval.rs:1666`) needs all of:
- no `?` and no negation;
- a past participle such as renewed, purchased, submitted, delivered,
  completed, finished, booked or reinstated;
- no active plan;
- `life.observe`, `life.commit` and `life.recall` all present in the turn's
  tool assembly.

That last point matters because `life.steward` is only on-demand in the
orchestrator profile (`main.rs:4325`).

**Steps:**
1. **Diagnose (S).** On vps-jane, check whether Beacon's sessions project
   `life.steward` (`ipc.rs:15017-15038`). If the skill isn't active, the
   procedure never binds.
2. **Write `scripts/smoke-procedure-run.sh` (S).** On an isolated hotel profile:
   - drive an IPC turn "I submitted the renewal form" against a seeded open loop;
   - assert `plan_seeded`, the guidance marker
     `[Procedure guidance: outcome-reflex @ life.observe]`, and one
     `procedure_run` row with `procedure_id=outcome-reflex`;
   - assert `phil procedure runs outcome-reflex` shows it.
   This covers P1–P3 at the SMOKE-GREEN level.
3. **Live P1–P3 (S).** The operator sends Beacon one natural outcome report
   about a recalled open loop. Record the row, which moves P1–P3 to
   watched-live.
4. **P4 trial gate (M).**
   - Set `PHILOTIC_PROCEDURE_TRIAL_RUNS=2` on vps-jane for the trial window.
   - With operator approval, stage one blocked run (resolving a loop that
     doesn't exist) and one complete run at v1.
   - Then: contrast whisper → Pending patch → `phil procedure approve` →
     two trial runs → decide.
   - Revert `TRIAL_RUNS` after the window.
5. **Widen the trigger (follow-up, S).** Add the missing past participles
   (paid, sent, signed, scheduled, cancelled, returned, fixed) with tests in
   `plan_eval.rs`. Natural traffic then produces runs without a staged demo.

### W4 — Memory hygiene proof (S)

1. Make it configurable:
   - add an ansible var `philotic_memory_hygiene_enabled` and a template line
     in `philotic-hotel.service.j2` next to `PHILOTIC_DREAM_SWEEP_ENABLED`
     (line 47);
   - on mac-jane, check whether the launchd plist already sets
     `PHILOTIC_MEMORY_HYGIENE_ENABLED`. DEF-067 shows it ran on 07-20.
2. Enable it on **vps-jane only**. It holds the Cortex, so its sweep sees the
   real vaults.
3. Watch one 03:00 UTC run. It is proven when:
   - the journal shows `memory.hygiene: sweep complete` with `vaults_scanned > 0`;
   - the `memory_hygiene:last_run:vps-jane` config key is set;
   - `phil autonomy status --lane memory.hygiene` shows the run;
   - `memory.delta_digest` shows the last-run marker.
4. Run it for 7 nights, then decide: keep (move to verified) or turn off and
   retire.

### W5 — Relocation R6 proof (L)

1. **Prerequisites:**
   - fix mac-jane's peer ports in `ansible/host_vars/jane-vps.yml:57-61`
     (24849–24851 → the real 16370–16371 range); confirm with
     `just vps-port-drift-check`;
   - measure vps headroom (RSS and disk) for a Björk orchestrator plus a
     Telegram seat;
   - confirm the blob-bind deploy is on vps (it went out with the 10-01 deploy).
2. **Write `scripts/smoke-relocate.sh` (M).** It runs a mac↔mbp relocation of
   a test role with no transport. It asserts the
   INTENT→FEASIBILITY→STANDBY→CONTINUITY→SWITCH→RECONCILE→CLOSE state walk via
   `RelocateHotelStatus`, that the parked turn survives (R5), and that the
   sealed token moved (R7). Add `phil hotel relocate-status`, since no CLI
   exists.
3. **Live R6.** From one Telegram turn, move the Björk orchestrator and
   Telegram transport mac-jane → vps-jane, with the Architect pinned. The next
   message must be answered from vps-jane, and a whisper must round-trip.
4. **Move back.** It needs the same ceremony in reverse.
5. **Update the proposal disposition** with per-slice SVER levels: R2
   watched-live (09-22), R5, R6 and R7 as measured.

**Note:** DEF-171 (placement gossip LWW, see `PERIMETER_ENFORCEMENT_PROPOSAL.md`
P5) affects relocation's trust model. R6 is safe to run, because placement
never authorizes secret release, but record the caveat.

### W6 — Autonomy and decisions proof (S–M)

- **A9 outcome sweep.** Always on, at 04:00 UTC. Take snapshots of
  `phil autonomy pending` and `status` on day 0 and day 7, and record the
  Pending → Neutral transitions. It is passive.
- **A4 architect charter.** Operator decision:
  - enable on mac-jane (`PHILOTIC_ARCHITECT_CHARTER_ENABLED`, `_AGENT`,
    `_CHAT_ID`) and watch three 13:00 briefs; or
  - retire the in-repo path in favor of the live-DB charters.
- **Decisions D2 shadow.** The first deliverable is the agreement report that
  promotion (D4) needs:
  - `phil decisions report [--since]`, reading `decision_traces`
    (`ansible-mesh-core/src/decision_trace.rs:129`, `summarize()` at `:271`);
  - it shows judge-vs-rule agreement per call site.
  - No posture change in this proposal.

### W7 — Readiness check in `phil doctor` (M)

Add a `watch-live` group to `crates/philotic-web/src/doctor.rs`:

- **`runtime.binary-freshness`:** for each running guest, its binary mtime and
  sha, the hotel's `build_sha` (from `RELEASE_TRAIN_PROPOSAL.md` R2) against
  `origin/develop` or the expected release tag, and the process start time
  against the binary mtime.
- **`flags.opt-in`:** the effective value of every opt-in flag:
  - `PHILOTIC_MEMORY_HYGIENE_ENABLED`, `PHILOTIC_DREAM_SWEEP_ENABLED`,
    `PHILOTIC_MEMORY_SLEEP_MUTATE`;
  - `PHILOTIC_ARCHITECT_CHARTER_ENABLED`, `philotic_shadow_decisions_enabled`;
  - `PHILOTIC_HEAL_DECISIONS_FALLBACK`, `PHILOTIC_LIFE_HYGIENE_ENABLED`;
  - any `PHILOTIC_AUTONOMY_DISABLE_*`.
- **`autonomy.activity`:** for each lane, the last audit time and any pending
  count older than 7 days.
- **`procedures.activity`:** the last `procedure_run` per procedure.

Then the Runtime Truth Rule's first three checks are one command.

## Cadence

- One **weekly burn-down session**, about 1 hour:
  - walk `WATCH_LIVE_BACKLOG.md`;
  - prove or retire at least 3 items;
  - record proofs with `graph_record_test_run` / `graph_advance_verification`.
- **Proposal freeze.** While the backlog has more than 10 items, new proposal
  docs need an explicit operator request. Agents append to existing proposals
  instead.

## Ordering

```
W0 ─► W1 ─► W2
W0 ─► W3, W4, W6 (parallel) ;  W5 after the port fix ;  W7 any time (R2 of release-train helps)
```

## Definition of done

- The backlog file has 5 items or fewer, and none is older than 4 weeks.
- No DEFECTS row uses an out-of-vocabulary status.
- The lint is blocking in `pr-check`.
- Watched-live evidence is in the Intel Graph for P1–P3, memory hygiene, R6
  and A9.
- `phil doctor` reports readiness on all three hotels.

## Operator decisions needed

1. Approve the proposal freeze while the backlog has more than 10 items.
2. P4: allow a staged blocked run, which is a deliberate failure, to exercise
   the refiner gate?
3. A4 architect charter: enable on mac-jane, or retire the in-repo path?
4. W5: when is a mac-jane → vps-jane relocation window acceptable, given that
   Telegram traffic moves too?
