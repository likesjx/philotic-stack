---
title: Skill Governance Hardening — Risk-Tiered Approval, Resumable Plans
doc_type: proposal
domain: agent-loop
status: implemented
disposition: implemented
last_updated: 2026-08-31
tags:
  - skilldag
  - approval
  - governance
  - toolset
  - carryover-plan
proposal_id: skill-governance-hardening
implements: []
implemented_by:
  - PR TBD (codex/skill-governance-hardening)
active_seams:
  - skill-register-approval-gate
  - approval-timeout-carryover
---

# Skill Governance Hardening

**Why now:** live incident 2026-08-27 — Bjork's plan to register four narrow, low-risk
"music stewardship" tracking skills never registered a single one. Traced (see
`philote_checkpoint_clock_loss.md` follow-up conversation) to two compounding design
gaps, both structural rather than incidental:

1. `skill.register` is **unconditionally** gated — every call demands a fresh live
   Telegram approval, with no risk gradient. A skill that only wraps tools the
   registrant already holds (e.g. a practice-tracker built on `life.observe`) gets the
   identical maximal-friction treatment as one that could grant `bash.exec`. "Trust for
   session" cannot help — the unconditional gate is explicitly immune to policy.
2. A `WaitingApproval(parked)` eviction (operator didn't answer in time) discarded the
   turn's `active_plan` with **no carryover** — the one eviction path DEF-092–095's
   plan-carryover machinery didn't cover. The next user message restarted the whole
   goal from step 1, not step 2. Three evictions that evening, three restarts, zero
   skills landed.

## What SkillDAG already gets right

The skill model (`AbstractSkillRecord`: `implied_tools` + `implied_classes` +
`allowed_skills` DAG edges + lifecycle `validation_state` + append-only audit trail on
every register/update/assign/revoke/set_state) is sound and matches what "keep tools
and skills under control" needs. The gap was never the DAG — it's that `skill.register`
treated every registration as maximal risk regardless of what it actually grants, and
`skill.assign` (the real capability-*grant* act, attaching a skill to a role's toolset
profile) already sits behind `require_skill_admin` — a coarse identity gate, not a live
approval, but a real one. No new authorization primitive was needed there.

## Fixes

### 1. Risk-tiered `skill.register` gate
A registration whose declared `allowed_tools`/`allowed_classes` are already a subset of
the registering session's own current `bindings` (`effective_toolset` +
`allowed_classes`) downgrades from the unconditional gate to the normal
policy-governed approval path (`auto_approve_all`, preapproved classes, session trust
all apply normally — this removes the *unconditional floor*, it does not itself grant
silence). Deliberately conservative: **any** declared `allowed_skills` (SkillDAG edges)
keeps the call unconditional — resolving those transitively needs the hotel's graph,
which philote does not hold locally, and treating an unresolved edge as "grants
nothing" would be a false-negative risk read. `skill_register_call_within_bindings` in
`tool_exec.rs`, unit-tested.

### 2. Numbered approval cards
When a plan has more than one not-yet-done step that will hit the *same* unconditional
gate, the approval reason names the count ("this plan has N steps that each need this
same live approval — expect this card to repeat"). Fixes the confusion where an
operator, having just approved one card, let the next identical-looking one time out
assuming it was a duplicate.

### 3. Approval timeout resumes, not restarts
On a `WaitingApproval(parked)` (or plain `WaitingTool`/other-phase) eviction, the
evicted turn's `active_plan` — from whichever of `parked_approval_turn` or `active_turn`
carried it — is stashed into `state.carryover_plan` (preserving any existing
stall/continuation budget accounting) *before* the turn is cleared. The pre-existing
`resume_carryover_after_failed_turn` call at the eviction site already existed but had
nothing to resume, since nothing upstream of it ever created a carryover from a
hard-evicted turn. `verified_step_ids` carries forward only steps the turn's own
`plan_steps_verified` actually backed with a real tool result — never the model's own
step-status claim, matching the carryover contract's existing verification discipline.

### 4. Timeout wording distinguishes from a generic hang
The unblock notice for an approval-phase eviction now says the request "timed out
waiting for your answer" and that the next message resumes rather than starting over,
instead of the generic "I seem to have gotten stuck" used for every eviction kind.

## Explicitly out of scope (do not build without a fresh incident motivating it)
- A hotel-side risk gate on `skill.assign` — already behind `require_skill_admin`; no
  live gap found.
- Batch-approve-all UI (a single button clearing N pending cards at once) — the
  numbering + resume fixes solve the *lost-plan* failure mode without a new Telegram
  UI surface. Revisit only if numbered+resumable turns out insufficient in practice.
- Resolving `allowed_skills` DAG edges client-side for the register-gate downgrade —
  would need a hotel round-trip; the conservative "any edge stays unconditional" rule
  covers the common case (leaf skills) cleanly.

## Verification
- `skill_register_within_bindings_subset_check` (tool_exec.rs): covered/uncovered
  tools, covered/uncovered classes, any DAG edge forces unconditional, empty grant is
  trivially within-bindings.
- `approval_timeout_eviction_stashes_plan_into_carryover` (runtime.rs): parks a 3-step
  plan (1 done+verified, 2 pending) mid-`skill.register`, backdates past the deadline,
  runs the real watchdog, asserts the carryover survives with the correct
  `steps_done`/`verified_step_ids`, and asserts the unblock notice names the timeout
  and resumability.
- `cargo test -p philote`: 532 green (530 prior + 2 new). `cargo check --workspace`
  clean.
