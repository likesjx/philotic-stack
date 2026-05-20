---
title: Cognitive Loop v2 — Plan Gate, Anti-Loop, and Earned Permissions
doc_type: proposal
domain: runtime-sessions
status: implemented
last_updated: 2026-05-20
tags:
- agent-loop
- plan-gate
- tool-dedup
- approval-policy
- earned-permissions
- philote
related_docs:
- COGNITIVE_LOOP_PROPOSAL.md
- APPROVAL_UX_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: cognitive-loop-v2
active_seams:
- context-envelope-contract
- active-plan-streaming
- rules-tier
---

# Cognitive Loop v2 — Plan Gate, Anti-Loop, and Earned Permissions

## Problem Statement

Observed in Beacon (and reproducible in any philote): when a philote executes
multi-step plans that include role-management or configuration tools
(`role.configure`, `role.create_or_update`), it enters a spin loop that only
terminates at the iteration cap (default 20). Root causes identified via code audit:

1. **Re-entry instruction biases toward more tool calls.** `project_working_state`
   ends every re-entry with _"Call another tool if needed, or respond to the user if
   you have enough information."_ After a successful step the model has no clear signal
   that the plan is done, so it calls the same tool again.

2. **`ActivePlan` step status is never advanced by the runtime.** The model must
   write updated step statuses in its reply text. On re-entry it sees stale
   `status: "pending"` and re-executes completed steps.

3. **No deduplication guard.** `working_tool_history` grows unboundedly. If
   `(tool_name, arguments)` already appears with a success result, the runtime will
   happily dispatch it again.

4. **Minimal tool result messages.** `"Role 'X' created/updated successfully."` gives
   the model no structured confirmation of what was actually persisted. Ambiguity
   drives retries.

5. **No plan-before-fire gate.** The model jumps straight to tool execution. The
   operator has no opportunity to redirect or constrain the approach before side-
   effects land.

6. **No earned-permissions path.** The only way to widen pre-approval is a manual
   `/preapprove` command. The model cannot propose standing permission tied to
   observable criteria (e.g. "after N clean executions").

---

## Slices

### Slice 1 — Anti-Loop: Dedup Guard + Rich Re-entry Framing  *(ship first)*

**What ships:**
- `working_tool_history` dedup check before dispatch: if `(tool_name,
  canonical_args)` already appears in history with a non-error result, inject a
  `provider_repair_note`-style warning into the re-entry envelope instead of
  dispatching again.  The model sees: _"Warning: `{tool}({args})` already
  succeeded at step N. Do not repeat it unless the prior result was an error."_
- Replace the generic re-entry footer in `project_working_state` with a structured
  summary:
  - Which plan steps are `done` / `in_progress` / `pending` / `failed`
  - What the last tool result was (success or error)
  - Explicit stop signal when all plan steps are `done` or no plan is active
    and the last step succeeded
- When `active_plan.status == "done"` in the model's latest response, the loop
  delivers the final reply immediately without re-entering.

**Files touched:**
- `crates/philote/src/session/mod.rs` — `project_working_state`, `push_tool_history`
- `crates/philote/src/runtime.rs` — `handle_tool_result` (dedup check before
  `build_reentry_context_envelope`)

**Seams:**
- `context-envelope-contract` — working_state section format changes
- `active-plan-streaming` — plan.status == "done" terminates loop

---

### Slice 2 — Plan Gate: Discuss Before Fire

**What ships:**
- New `TurnPhase::PlanningDiscussion` phase.
- New `AgentAction::PlanProposal { summary, steps, advisory }` parsed from model
  output when `kind: "plan_proposal"`.
- When a `PlanProposal` is received:
  - Surface the plan to the user via `send_reply` with a formatted summary +
    _"Reply to proceed, or redirect me."_
  - Park the turn (same mechanism as `WaitingApproval` / `park_active_turn_for_approval`).
  - On the next user message, restore the parked turn and re-enter with
    `plan_confirmed: true` injected into the working state.
- Tools tagged `class: "planning"` bypass the plan gate — they fire freely in
  the discussion phase (e.g. `memory.recall`, `echo`, `memory.remember`).
- The model schema for `plan_proposal` includes:
  ```json
  {
    "kind": "plan_proposal",
    "summary": "...",
    "steps": [{"id":1,"description":"...","tool_name":"..."},...],
    "advisory": { "approval_risk_hint": "low|medium|high" }
  }
  ```

**Files touched:**
- `crates/philote/src/loop.rs` — add `PlanProposal` variant to `AgentAction`,
  parse in `interpret_model_payload`
- `crates/philote/src/session/types.rs` — add `PlanningDiscussion` to `TurnPhase`,
  `plan_confirmed: bool` to `WorkingTurn`
- `crates/philote/src/session/mod.rs` — `project_working_state` surfaces
  `plan_confirmed` context; watchdog timeout for PlanningDiscussion
- `crates/philote/src/runtime.rs` — `handle_plan_proposal`, park/restore logic,
  watchdog arm for `TurnPhase::PlanningDiscussion`

**Seams:**
- `context-envelope-contract`
- `active-plan-streaming`

---

### Slice 3 — Earned Permissions

**What ships:**
- `conditional_preapprovals: Vec<ConditionalPreapproval>` added to `ApprovalPolicy`.
- `ConditionalPreapproval` shape:
  ```rust
  pub struct ConditionalPreapproval {
      pub tool_name: String,
      pub condition: PreapprovalCondition,
      pub granted: bool,
      pub granted_at: Option<u64>,
  }
  pub enum PreapprovalCondition {
      SuccessiveSuccesses { threshold: u32, count: u32 },
      OperatorStanding { reason: String },
  }
  ```
- New planning-class tool `approval.request_standing(tool_name, condition,
  threshold, rationale)`:
  - Surfaces a standing-permission request to the operator (same channel as a
    regular approval request).
  - On operator approval, inserts a `ConditionalPreapproval` into the session's
    `ApprovalPolicy`.
- Runtime tracks `SuccessiveSuccesses.count` automatically on each clean tool
  execution. When `count >= threshold`, the condition auto-grants and the runtime
  notifies the operator.
- Granted conditionals are persisted in the apartment checkpoint alongside the
  rest of `ApprovalPolicy`.

**Files touched:**
- `crates/philote/src/session/types.rs` — `ConditionalPreapproval`, `PreapprovalCondition`
- `crates/philote/src/session/mod.rs` — `approval_policy_allows` checks conditionals;
  `push_tool_history` increments `SuccessiveSuccesses.count`
- `crates/philote/src/runtime.rs` — `execute_local_agent_tool` wires
  `approval.request_standing`; notifies operator when threshold is met
- `crates/philote/src/catalog.rs` — expose tool definition for `approval.request_standing`

**Seams:**
- `rules-tier` — conditionally granted permissions feed into approval policy
  alongside rules

---

## Non-Goals (for this workstream)

- `rule.audit` tool (self-improvement feedback loop) — deferred; depends on Slice 3
  being stable first.
- `settings.adjust` planning tool — deferred; existing `/agent configure` covers
  this for now.
- Per-step tool history structural overhaul (native message format) — a larger
  model-router protocol change; tracked separately.

---

## Implementation Order

```
Slice 1  →  Slice 2  →  Slice 3
(unblock Beacon immediately)
```

All three slices live on `codex/cognitive-loop-v2`.

---

## Acceptance Criteria

**Slice 1**
- [ ] A philote that successfully creates a role does not re-call the same role
      tool in the same turn
- [ ] When `active_plan.status == "done"`, the turn closes without another model
      round-trip
- [ ] Re-entry working state summary shows per-step status, not just a generic footer

**Slice 2**
- [ ] A turn entering `PlanningDiscussion` surfaces the plan to the user and parks
- [ ] User reply resumes execution with `plan_confirmed: true` visible in working state
- [ ] Planning-class tools (`memory.recall`, `echo`) fire freely without gating
- [ ] Watchdog times out a `PlanningDiscussion` turn at `WAITING_APPROVAL_SECS`

**Slice 3**
- [ ] `approval.request_standing` surfaces a request and operator can approve/deny
- [ ] `SuccessiveSuccesses` auto-grants at threshold and notifies operator
- [ ] Conditional grants survive restart via checkpoint round-trip
