---
title: Self-Improvement Loop — Mechanical Triggers, Curation, and Fail-Loud Memory Under Philotic Gates
doc_type: proposal
domain: runtime-sessions
status: accepted-current-slice
disposition: accepted-current-slice
last_updated: 2026-09-03
tags:
  - self-improvement
  - skills
  - skilldag
  - memory-context
  - autopoiesis
  - hermes-agent
  - cron
  - prompt-guard
related_docs:
  - AUTOPOIESIS_PROPOSAL.md
  - SKILL_GOVERNANCE_HARDENING_PROPOSAL.md
  - SKILL_LIFECYCLE_PROPOSAL.md
  - DATA_DRIVEN_TOOL_GRANTS_PROPOSAL.md
  - WHISPER_PROTOCOL_PROPOSAL.md
  - MEMORY_TRANSPARENCY_PROPOSAL.md
  - DISTRIBUTED_CRON_PROPOSAL.md
  - COGNITIVE_LOOP_V2_PROPOSAL.md
  - MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md
  - ARCHITECTURE_STATUS.md
task_refs:
  - docs/task.md
proposal_id: self-improvement-loop
implements: []
implemented_by: []
active_seams:
  - skills-distill-trigger
  - skills-curate-sweep
  - skill-patch-pending-queue
  - standing-notes-budget
  - prompt-guard-scan
  - cron-continuity
  - session-search-fts
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
---

# Self-Improvement Loop

**Why now:** a side-by-side read of Nous Research's Hermes Agent v0.21.0
("Pantheon", 2026-08-31) against Philotic on 2026-09-03 showed the two systems
have the *same* organs for learning from experience and opposite failure modes.
Hermes writes skills and memory freely and worries about sprawl. Philotic gates
every write well — `SkillValidationState`, the DEF-103 risk-tiered
`skill.register` approval, SVER, Muninn provenance — and then **nothing pulls
the trigger.** Grep-confirmed on develop `861af236`:

- the only "when to create a skill" rule is prose in `skills/skill-authoring/SKILL.md`
  ("3 or more times"); no code counts anything;
- `AbstractSkillRecord` (`ansible-mesh-core/src/graph.rs:157`) has no
  last-invoked time, no invocation count, and no creator provenance, so no sweep
  can ever retire an unused agent-authored draft;
- `skill.register` is the only write path and it upserts the whole record
  (`aiua/src/service/ipc.rs:16422`), so every refinement is a full re-approval;
- `memory_integration.rs` scans nothing it promotes into a prompt for injection
  or exfiltration patterns (the July 2026 hermes/OpenClaw analysis flagged the
  same gap; only `exec-guard` — itself ported from Hermes' `approval.py` — landed);
- `CronJob` (`ansible-mesh-core/src/cron.rs:105`) carries no previous-run output,
  which is the structural reason the first silence-as-signal detector filed
  5,947 alerts in an hour before it was bounded (S6);
- there is no full-text index over `session_turn` nodes; a philote that wants
  to remember what it discussed last month pages the graph, which is the access
  pattern behind DEF-080.

This is the last-mile problem the Autopoiesis proposal names ("every loop in
the system currently terminates in prose or in a human"), scoped to the
philote's own learning loop. Autopoiesis Slice A7 (`skills.register_learned`)
already reserves the *repeated success* trigger; this proposal supplies the
mechanics around it and the neighbours A7 needs to be safe: curation, cheap
refinement, bounded standing memory, a scan on the way back into the prompt,
and continuity for scheduled runs.

## What Philotic already gets right (keep, do not re-derive)

- **Gating.** `skill.register` is human-gated with a mechanical downgrade when
  the declared tool surface is already a subset of the caller's bindings
  (`philote/src/tool_exec.rs:145`, `:503`). Hermes' equivalent
  (`skills.write_approval`) is a boolean; ours is a gradient. Keep the gradient.
- **Lifecycle states with meaning.** `Draft → Validated → Registered → Active →
  Suspended{reason} / Invalid{errors} / Deprecated` (`graph.rs:117`) already
  expresses everything a curator needs. Hermes has stale/archived flags only.
- **The autonomy contract.** Every lane here is an `AutonomyGrant` lane
  (`ansible-mesh-core/src/autonomy.rs:197`): auditable, reversible, budgeted,
  never starting above ConfirmFirst, per-lane kill switch. Hermes has a
  curator pause/pin; we have a ledger.
- **Progressive disclosure.** `ToolsetProfileRecord.on_demand_skills` already
  keeps domain skill groups out of the visible tool list until a turn activates
  them. Parity with Hermes' name-and-description-only skill index; no work.
- **Three memories, not four.** Repo/docs/code for truth, the intel graph for
  structure, Muninn for continuity, `session_turn` for episodes. Hermes' four
  layers (MEMORY.md, USER.md, FTS5 sessions, external providers) is the part
  reviewers call fragile. Do not add a layer; bound the ones we have.

## The borrow, precisely

| Hermes mechanism (v0.21.0) | What it actually does | Philotic lane |
|---|---|---|
| Distill triggers | After a turn with ≥5 tool calls, an error recovered, a user correction, or a non-obvious path that worked, a background review may `skill_manage create/patch` or write memory. Mid-session a system-level nudge asks "anything worth persisting?" | L1 `skills.distill` |
| Curator (`agent/curator.py`) | Agent-created skills go stale at 30 days and archive at 90; never deleted; hand-authored skills untouched; `curator pause` / `pin`. Scoped by provenance. | L2 `skills.curate` |
| `patch` over `edit`; staged writes | `skill_manage patch(old_string,new_string)` is the preferred refinement; with write approval on, writes are staged under `~/.hermes/pending/skills/`, survive restarts, and are reviewed with `/skills pending\|approve\|reject`. | L3 `skill.patch` + pending queue |
| Hard-capped, fail-loud memory | `MEMORY.md` 2,200 chars + `USER.md` 1,375 chars, injected as a frozen snapshot with a usage percentage; a write over the cap returns an error instead of compacting. | L4 standing-notes budget |
| Scan on the way back in | Memory entries and installed skills are scanned for injection, exfiltration, destructive commands, and supply-chain signals; a `dangerous` verdict is not overridable; instruction files (AGENTS.md, skills, memory) always require write approval. | L5 `prompt-guard` |
| Cron continuity | `continuity=true` carries each run's output into the next; every job has a durable notepad; cron agents load and update memory like any other. | L6 cron continuity |
| Episodic search | Every session is in SQLite with an FTS5 index; the agent searches past context instead of loading it. | L7 `session.search` |

Everything else in Hermes' loop — skill hubs and taps, seven external memory
providers, free-running skill writes by default, GEPA-style prompt evolution —
is explicitly **not** borrowed (see "Do not copy").

## Slices

| Slice | Content | Size | Verify |
|---|---|---|---|
| L1 `skills.distill` — turn-close distill trigger | At `complete_agent_response` (`philote/src/turn_loop.rs:1723`), evaluate three mechanical predicates on the closing turn: (a) `working_tool_history.len() ≥ 5` (`session/mod.rs:1788`); (b) the turn passed through a provider/tool retry path and then succeeded (`turn_loop.rs:1400-1459`); (c) the user's message was corrective (a small lexical classifier: "no,", "not that", "wrong", "I meant", an explicit undo). If any fires and the lane budget allows, emit a **lookaside distill whisper** through the existing paracrine path (`IpcRequest::ParacrineEmit`, `paracrine.rs:858`) to the philote's own `distiller` role with the turn's tool history and outcome as the exosome. The distiller's only legal outputs are (1) a `skill.register` call that lands in `Draft` — never `Registered` — and (2) a Muninn write via the existing memory tools. Draft skills carry `skill_markers: [agent_authored, distilled]` and `field_sources.trigger` naming which predicate fired. Relation to A7: A7 keys on plan-eval repeat (≥3 completed plans with the same sequence); L1 keys on a single hard-won turn. Both feed the same `Draft` pool and the same operator approval; A7's ≥3-repeat count becomes the promotion hint from `Draft` to `Validated`. Posture: ProposalOnly forever (a Draft is a proposal). Budget: 3 distill whispers per agent per day; a whisper that produces nothing costs one. Kill switch `PHILOTIC_AUTONOMY_DISABLE_SKILLS_DISTILL`. | M | test-green (each predicate has a fixture turn; over-budget turn emits nothing) + watched-live: one Draft skill appears in `phil skill list --state draft` after a real ≥5-tool turn on mac-jane, and the operator can read which predicate fired |
| L2 `skills.curate` — provenance-scoped staleness sweep | Extend `AbstractSkillRecord` with `provenance: SkillProvenance { Repo, Operator, Agent{agent_id} }`, `last_invoked_at: Option<u64>`, `invocation_count: u64` (defaulted for old records; `Repo` for anything with `repo_skill_path` in `field_sources`). Stamp `last_invoked_at` where a skill's implied tools are activated in `ToolAssembly`. Add a nightly `internal:skill_curate_sweep` job following the `fire_memory_hygiene` pattern (`aiua/src/service/cron_ticker.rs:595`, `memory_hygiene::run_scheduled_sweep`): agent-authored skills in `Draft`/`Validated` with no invocation for 30 days → `Suspended{reason: "stale-30d"}`; suspended for a further 60 days → `Deprecated`. **Never deletes.** `Repo` and `Operator` provenance are never touched. `skill_markers: [pinned]` exempts a record. Every transition writes a `skill_registration_audit` node. Reversal is `skill.set_state` back to `Validated`, which resets the clock. Posture: AutoWithAudit is acceptable from day one because the action is reversible and touches only agent-authored records; freeze on 3 operator reversals. `phil skill list` gains `--stale` and `--provenance`. | S–M | test-green (fixture skills at 29/31/91 days per provenance; pinned skill untouched; audit row per transition) + smoke-green: sweep runs on mac-jane, `phil skill list --stale` shows the result, `phil autonomy status` shows the lane |
| L3 `skill.patch` + durable pending queue | New tool + IPC `skill.patch { skill_name, field: description\|goal_template, old_string, new_string }` that rewrites only text fields. Risk tier under the DEF-103 gate: a patch cannot change `implied_tools`/`implied_classes`/`allowed_skills`, so `skill_register_call_within_bindings` is trivially satisfied and the call is **normal policy-governed** (Trust-for-session applies); patches to a skill the caller does not own stay unconditional. Every `skill.register`/`skill.patch` that parks for approval is persisted as a `skill_registration_pending` node (new kind beside `skill_registration_audit`, `domain/kinds.rs:26`) carrying the full payload, the requesting session, and the approval card text — so an approval-timeout eviction (DEF-103's carryover) has a durable object to resume, not just a stashed plan. philotic-web gains `GET /api/skills/pending`, `POST /api/skills/pending/:id/approve\|reject` next to `handle_skills` (`philotic-web/src/serve.rs:3860`), and the desktop skills view lists them. Telegram approval cards link to the pending id. | M | test-green (patch on own skill → policy path; patch on other's skill → unconditional; pending node survives a simulated guest restart and is resumable) + watched-live: a real patch approved from the desktop view lands without a Telegram round-trip |
| L4 standing-notes budget | Philotic already has a `memory_snapshot_chars` slot in `InjectionBudget` (`philote/src/session/types.rs:1289`). This slice makes that layer behave like Hermes' MEMORY.md/USER.md pair: two per-philote blocks — `agent_notes` (environment facts, conventions, things learned; cap 2,200 chars) and `operator_profile` (preferences, communication style, expectations; cap 1,375 chars) — stored as graph nodes owned by the agent, edited only through `notes.add\|replace\|remove` (substring `old_text` matching, mirroring Hermes so the model behaviour is well-trodden), and **fail-loud**: a write that would exceed the cap returns an error naming the overage; there is no auto-compaction. Injected as a frozen snapshot at turn start with `[notes 71% · profile 40%]` in the header so the model sees pressure. Distinct from Muninn recall (semantic, unbounded, retrieved) and from the dialogue window (episodic, DEF-102 budgeted). The first slice task is to read what `memory_snapshot` holds today and either adopt or replace it — the proposal does not assume. | M | test-green (cap enforcement, error text, substring replace, both themes of injection header) + watched-live: a philote records an operator preference into `operator_profile` unprompted and it survives a checkpoint restore |
| L5 `prompt-guard` — scan on the way back in | A sibling crate to `exec-guard` with the same shape (`detect_hardline(text) -> Option<HardlineMatch>`, `exec-guard/src/lib.rs:81`) but for text that will be rendered into a prompt: instruction-override phrasings, tool-call smuggling, credential/exfil URLs, base64 blobs, zero-width and bidi characters, and the `exec-guard` hardline set itself (a skill goal that embeds `rm -rf` is dangerous whether or not it ever executes). Gate points: `skill.register`/`skill.patch` `description`+`goal_template` (in `handle_register_skill`, `ipc.rs:16422`), `memory.promote_candidate` (`memory_integration.rs:2576`), L4 note writes, and `ReportMcpUpstreamCatalog` tool descriptions (annotate, do not block — provenance rendering already exists). Two verdicts: `dangerous` → rejected, not overridable, audit row; `caution` → forces the unconditional approval tier regardless of subset check. No model in the loop; patterns only, like exec-guard, so it is testable and cheap. | S–M | test-green (corpus of 50 positive / 50 negative fixtures, including the DEF-089 token-in-URL shape) + smoke-green: `phil doctor` reports scan counts per gate point |
| L6 cron continuity | `CronJob` gains `continuity: bool` and a per-job `cron_scratch` node (bounded, 4,000 chars, last-writer-wins like everything else) written by the fired turn's final reply. When `continuity` is set, `build_cron_task_json` (`cron_ticker.rs:844`) injects the previous run's scratch as `[Previous run]` in the task payload so a monitor can dedupe against what it already reported, and a research job can continue instead of restarting. `cron.register`/`cron.list` expose the flag; `cron.list` shows scratch age. Ties to S6: a detector with memory of its own last report cannot re-alert on the same silence. | S | test-green (two consecutive fires, second sees first's output; scratch bounded; disabled flag injects nothing) + watched-live: the existing Chronos check-in on vps-jane with `continuity: true` references its previous brief |
| L7 `session.search` — FTS5 over session turns | Add a SQLite FTS5 virtual table (`session_turn_fts(turn_id, agent_id, ts, text)`) maintained by the hotel alongside `graph_nodes` (`sqlite_storage.rs:304`) for `session_turn` nodes, plus IPC `SearchSessionTurns { agent_id, query, limit ≤ 20 }` and philote tool `session.search`. Results are excerpts with turn ids, never whole turns, under the existing `recalled_memory_chars` budget. Backfill is a one-shot migration bounded per hotel. FTS5 is already compiled into the workspace (`graph-intelligence/src/engine.rs` uses it for the project graph); the hotel context DB has none. This is the cheap episodic recall Hermes gets from `state.db`; for Philotic it also closes the DEF-080 access pattern (scanning all history under the DB mutex) with an index instead of a scan. | M | test-green (index maintained on insert; query returns bounded excerpts; migration idempotent) + smoke-green on vps-jane: `session.search` over Beacon's history returns in <50 ms with the DB mutex held for the query only |

Dependency: L5 before L1 goes live (a distiller that can write Draft skills
must have its output scanned); L2 needs the `provenance` field from its own
first task before the sweep runs; L3 is independent; L4 independent; L6
independent; L7 independent. A7 (`skills.register_learned`) is **amended, not
replaced**: its "≥3 completed plans" signal becomes the `Draft → Validated`
promotion hint, and L1 supplies the single-turn trigger A7 never had. A9's
outcome stamps are the training signal for whether distilled skills are worth
keeping; L2 consumes `reversed` outcomes as an extra staleness signal.

Substrate rule inherited unchanged from Autopoiesis: no lane is promoted past
ConfirmFirst until `SUBSTRATE_HARDENING` S1–S3 are live, except L2, whose only
action is a reversible state transition on records the agent authored itself.

## Autonomy contract (inherited, with one addition)

1. Auditable, reversible, budgeted; never starts above ConfirmFirst; per-lane
   kill switch. (Autopoiesis rules 1–3, unchanged.)
2. **Provenance is a hard boundary.** No lane in this proposal may write to,
   suspend, or deprecate a `Repo` or `Operator` skill, a hand-edited note, or an
   operator-registered cron job. This is Hermes' curator rule and it is the
   reason the curator is trusted; adopt it verbatim.
3. Anything that lands in a prompt passes L5 first. A `dangerous` verdict is
   not overridable by any posture, grant, or "trust for session".

## Do not copy

- **Free-running skill writes.** Hermes creates skills by default without a
  gate; its own docs recommend `write_approval`. Philotic's `Draft` state is
  the staging area; nothing this proposal adds writes `Registered` or `Active`
  autonomously.
- **Skill hubs, taps, marketplaces.** Third-party skill ingestion is a supply
  chain, not a learning loop. Out of scope; if ever wanted it is a
  `perimeter-egress-control` concern with L5 as the floor.
- **External memory providers.** Muninn is the continuity layer. Eight
  pluggable memory back-ends is the complexity reviewers flag in Hermes.
- **Prompt evolution (GEPA-style).** Rewriting a philote's own charter from
  traces is A8b's territory and stays ProposalOnly forever there.
- **A fourth memory layer.** L4 bounds an existing slot; it does not add one.

## Verification

- Every slice ships with the fixtures named in its row; `just test-and-record
  proposal:self-improvement-loop` after each.
- Watched-live gates are per slice and named above. The proposal as a whole is
  watched-live-green when, on one hotel, a real turn produces a Draft skill
  (L1), the operator approves a patch to it from the desktop (L3), it is
  invoked by a second philote (A7's original gate), and the curator leaves it
  alone because it was used (L2).
- `phil autonomy status` must show `skills.distill` and `skills.curate` as
  lanes with real budgets before either is enabled on a second hotel.

## Disposition

`accepted-current-slice` — operator chose **L5 + L1** as the first slice on
2026-09-03 (`codex/self-improvement-l5-l1`). Implemented, test-green:

- **L5** `crates/prompt-guard` (exec-guard shape; `Dangerous` / `Caution`;
  hidden-char and embedded-hardline checks in code, 9 + 7 regex rules;
  100-fixture corpus). Gate points live: `skill.register` on both sides
  (philote `route_tool_call_execution` denies Dangerous before approval and
  pins Caution to the unconditional tier; hotel
  `handle_register_skill_with_origin` rejects Dangerous with a `rejected`
  audit row and records Caution in `field_sources.prompt_guard`) and
  `memory.promote_candidate` (Dangerous → `allowed:false`). The MCP-catalog
  annotation and the `phil doctor` scan counts named in the L5 row are **not**
  in this slice.
- **L1** `crates/philote/src/distill.rs`: three predicates at
  `deliver_text_reply`, lookaside whisper to the philote's own role with the
  new `ParacrineRouting::Discard`, `context.intent = skills.distill:<trigger>`
  recognised by the tool layer (fixed allowlist `skill.register`,
  `skill.list`, `memory.remember`, `memory.recall`; anything else is a
  tool-result denial), `RegisterSkill.origin` forces the hotel to `Draft` +
  `agent_authored`/`distilled` markers + `field_sources.trigger`. Budget via
  `ConsumeAutonomyAction { filing: true }` — a new flag that lets a lane
  *file* at `ProposalOnly` (budgeted, audited `Pending`); lane
  `skills.distill` defaults to 3/day per hotel. Kill switch
  `PHILOTIC_AUTONOMY_DISABLE_SKILLS_DISTILL`; role override
  `PHILOTIC_SKILLS_DISTILL_ROLE`.

**Reality gap found while implementing (feeds L2/L3):**
`SkillValidationState::is_projectable()` returns true for `Draft` — only
`Suspended`/`Deprecated` are excluded — so "a Draft grants nothing" holds
today only because a Draft is never *assigned* until an operator runs the
gated `skill.assign`. The distill turn's tool allowlist deliberately omits
`skill.assign`/`skill.set_state` for exactly this reason. L2 should make
`Draft` non-projectable explicitly rather than rely on the assignment gate.

Next: watched-live on mac-jane (one real ≥5-tool turn → a Draft skill visible
in the hotel graph with `origin=distill:*`), then L2 before the Draft pool
grows. Recorded in the intel graph on
`doc:autopoiesis-proposal` (observation + writeback items, 2026-09-03) and as a
decision on `seam:a2a-membrane-contract` for the Hermes interop half, which is
a separate proposal.
