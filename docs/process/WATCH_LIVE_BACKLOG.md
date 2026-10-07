---
title: Watch-Live Backlog
doc_type: process
domain: workflow-docs
status: active
last_updated: 2026-10-07
tags:
- verification
- watched-live
- backlog
related_docs:
- ../architecture/WATCH_LIVE_BURNDOWN_PROPOSAL.md
- WORKFLOW.md
- ../DEFECTS.md
- ../task.md
---

# Watch-Live Backlog

The single list of merged work that still needs a watched-live proof. Every
item ends **proven** (watched-live, with evidence recorded in the Intel Graph
via `graph_record_test_run` / `graph_advance_verification`) or **retired**
(flagged off or deleted, with a decision record). Process and slices:
[WATCH_LIVE_BURNDOWN_PROPOSAL.md](../architecture/WATCH_LIVE_BURNDOWN_PROPOSAL.md).
"Proof" follows the Runtime Truth Rule in [WORKFLOW.md](WORKFLOW.md): the
installed binary changed, the supervisor restarted, the process runs the new
binary, and the observed behavior came from that process.

Seeded 2026-10-07 (W0). Weekly burn-down: walk this table, prove or retire at
least 3 items, then delete the row and note the outcome in the proposal or
DEFECTS row it came from.

**Owner:** `operator` = needs hotel access, a live conversation, or an operator
decision; `cloud-agent` = repo-only work.

**Not on this list (unbuilt, back to `proposed`):** Self-Improvement L6,
Reflexive Life Graph R2–R5, Graph Doors G2. They return here only once built.

| Item | Proposal | Proof needed | Hotel | Blocker | Owner | Next action | Due |
|---|---|---|---|---|---|---|---|
| Procedural graphs P1–P3: first `procedure_run` row | PROCEDURAL_GRAPHS_PROPOSAL.md | A natural outcome report about a recalled open loop seeds a plan with `procedure_id`; `phil procedure runs outcome-reflex` shows one row | vps-jane (Beacon) | Zero rows on any hotel; narrow trigger (`plan_eval::reports_an_outcome`), and `life.steward` may not be projected | operator | Check whether Beacon's sessions project `life.steward`; send one natural outcome report (W3 steps 1–3) | 2026-11-04 |
| Procedural graphs P1–P3: smoke script | PROCEDURAL_GRAPHS_PROPOSAL.md | `scripts/smoke-procedure-run.sh` on an isolated profile asserts `plan_seeded`, guidance marker and one `procedure_run` row (SMOKE-GREEN) | isolated profile | None | cloud-agent | Write the smoke script (W3 step 2) | 2026-11-04 |
| Procedural graphs P4 trial gate | PROCEDURAL_GRAPHS_PROPOSAL.md | One blocked and one complete run at v1 → contrast whisper → Pending patch → `phil procedure approve` → two trial runs → decision | vps-jane | Operator decision 2 (allow a staged blocked run?); needs P1–P3 rows first | operator | Decide on the staged run; set `PHILOTIC_PROCEDURE_TRIAL_RUNS=2` for the window | 2026-11-04 |
| `memory.hygiene` nightly | MEMORY_TRANSPARENCY_PROPOSAL.md / MUNINN_MEMORY_CORE_PROPOSAL.md | Journal `memory.hygiene: sweep complete` with `vaults_scanned > 0`; `memory_hygiene:last_run:vps-jane` set; `phil autonomy status --lane memory.hygiene` shows the run; 7 nights | vps-jane | No deploy config enables it (no ansible var) | cloud-agent → operator | Add `philotic_memory_hygiene_enabled` + template line (W4 step 1), then operator enables on vps-jane | 2026-11-04 |
| Relocation R6 live move (+ R1/R3/R5/R7 smokes) | RELOCATION_CEREMONY_PROPOSAL.md | Björk orchestrator + Telegram transport mac-jane → vps-jane from one Telegram turn; next message answered from vps; whisper to Architect round-trips; move back. Smokes: R1 home survives restart, R5 parked turn survives, R7 sealed token moved | mac-jane → vps-jane | mac-jane peer ports wrong in `jane-vps.yml:57-61`; no `smoke-relocate.sh`; no `phil hotel relocate-status`; operator decision 4 (window) | operator (live) / cloud-agent (smoke + CLI) | Fix peer ports; write `scripts/smoke-relocate.sh` (W5) | 2026-11-04 |
| Model-catalog freshness (DEF-202/203/204) | — (DEFECTS.md) | After deploy: a `model_profile:*:vps-jane-aiua-01` row on mac and mbp; mac catalog refreshes within 6 h; two legacy vps rows deleted | all three | Deploy pending | operator | Deploy current develop to all hotels; delete the 2 legacy rows | 2026-11-04 |
| Heal H0/H1 (DEF-205/206/208) | DECISIONS_MODEL_PROPOSAL.md (heal slices) | Stale sweep moves escalations to `stale`; fallback tags appear in the journal (`PHILOTIC_HEAL_DECISIONS_FALLBACK` already on in `jane-vps.yml`) | all three (flag: vps-jane) | Deploy pending | operator | Deploy; grep journal for `stale` moves and fallback tags | 2026-11-04 |
| DEF-212 live revocation UAT | AGENT_FRONTDOOR_PROPOSAL.md | `mcp-client-uat.sh agent-frontdoor`: a revoked token stops working without a guest restart | vps-jane | None — the 2026-10-01 handoff reports it passed; **proven pending record** | operator | Record the UAT result in the graph and move DEF-212 to `fixed (verified)` | 2026-11-04 |
| Surface S1b buttons | DESKTOP_GENERATIVE_SURFACES_PROPOSAL.md | Ask Beacon for a status card with buttons on Telegram, tap one, Beacon handles the `[surface action]` | vps-jane (Beacon) | Deploy of S1b | operator | Deploy, then run the one-tap check | 2026-11-04 |
| MCP endpoint steward | MCP_MEMBRANE_GATEWAY_PROPOSAL.md (`mcp.endpoint_steward` skill) | A philote provisions an endpoint through the skill and serves a real external client call deterministically (smoke-green on mac-jane 2026-09-05; no watched-live record) | mac-jane | Proof criterion not yet written down in task.md | operator | Confirm the criterion, then run one real client call | 2026-11-04 |
| Integration steward (Hevy key) | OUTBOUND_INTEGRATIONS.md (`integration.steward` skill) | Coach `GET /v1/workouts?page=1&pageSize=1` → 200 and a polling cron registered; then a fresh "connect me to <vendor>" handled end to end | mac-jane (Coach) | Hevy key not provisioned | operator | `phil integration set-credential hevy-webhook-api --owner agent-coach --credential-file <file>` | 2026-11-04 |
| Lyra trip pass | LYRA_TRAVEL_AGENT_PROPOSAL.md | One research → structure → steward pass over a real trip idea lands `Project`/`Commitment`/`Event`/`NextAction` in the LifeGraph | mbp-jane (Lyra) | Needs a real trip idea from the operator | operator | Send Lyra one real trip idea | 2026-11-04 |
| Peer delegate inbound | — (task.md, DEF-151) | Beacon hands a task to Björk: refusal naming the agent or "queued, not confirmed", and a `peer.delegate` inbound on mac-jane | vps-jane → mac-jane | Passive | operator | Check after the next natural hand-off and record it | 2026-11-04 |
| Gardener slice 7 | — (task.md, Philote Say-Do + Continuity) | "Garden the LifeGraph": turn 1 ends 12/13, harness closer runs `life.audit`, `plan_eval complete` 13/13, before/after health score | mac-jane (Björk) | Passive | operator | Check after the next gardening pass and record it | 2026-11-04 |
| 11:00 brief ≤ 2 messages | — (task.md, DEF-113/121) | Next 11:00 UTC Beacon brief is at most 2 messages; replayed turns stamped; explicit `life.recall` returns packets | vps-jane (Beacon) | Passive | operator | Read the next brief and its journal | 2026-11-04 |
| Media evidence (DEF-163/164/165) | — (task.md, `codex/media-evidence-carry`) | (a) a refused photo reply still writes what the photo says; (b) caption-less photo after "send me X again" performs X; (c) model-authored `life.observe.batch` without packet_id lands first try | mac-jane (Björk) | Passive | operator | Check on the next photo turn and record it | 2026-11-04 |
| T1 tool catalog file | TOOL_MANAGEMENT_PLANE_PROPOSAL.md | Edited override description appears in Björk's prompt after a hotel restart; a `life.observe`-bound plan verifies from one `life.observe.batch` call | mac-jane | Passive | operator | Edit an override description, restart, inspect the prompt | 2026-11-04 |
| Guest supervisor 24 h soak | — (DEFECTS.md technical debt) | Supervisor on by default across a fleet deploy for 24 h: no respawn-budget exhaustion except real faults | all three | Needs the next fleet deploy | operator | Watch 24 h after the next fleet deploy | 2026-11-04 |
| A9 outcome sweep (7-day) | AUTOPOIESIS_PROPOSAL.md | Snapshots of `phil autonomy pending` and `status` on day 0 and day 7; Pending → Neutral transitions recorded | all three | Passive (runs 04:00 UTC) | operator | Take the day-0 snapshot | 2026-11-04 |
| A4 architect charter | AUTOPOIESIS_PROPOSAL.md | Enable on mac-jane (`PHILOTIC_ARCHITECT_CHARTER_ENABLED`, `_AGENT`, `_CHAT_ID`) and watch three 13:00 briefs — or retire the in-repo path | mac-jane | Operator decision 3 (proposed: enable on mac-jane) | operator | Decide; if enabling, set the three env vars | 2026-11-04 |
| Decisions D2 agreement report | DECISIONS_MODEL_PROPOSAL.md | `phil decisions report [--since]` shows judge-vs-rule agreement per call site from `decision_traces` (feeds D4) | vps-jane | Report command not built | cloud-agent | Build `phil decisions report` (W6), then operator runs it on vps-jane | 2026-11-04 |
| DEF-088…099 journal-grep closure | — (DEFECTS.md) | Journal greps for each row's log signature (e.g. DEF-088 draft creation, DEF-095 `plan_stopped` "superseded"); mark `fixed (verified)` or reopen | vps-jane / mac-jane | Needs journal access | operator | Run the greps and update the DEFECTS rows | 2026-11-04 |
