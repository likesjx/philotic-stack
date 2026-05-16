---
name: Cognitive Loop V2
description: Three-slice overhaul of philote cognitive loop — anti-loop, plan gate, earned preapproval — PR #58
type: project
---

PR #58 (`codex/cognitive-loop-v2` → `develop`) — shipped 2026-04-28

**Slice 1**: Anti-loop dedup + rich re-entry framing
- Dedup guard in `route_tool_call_execution`: skips re-dispatch if same (tool, args) already succeeded this turn
- Plan-done early exit in `handle_tool_result`: forces no-tool wrap-up when ActivePlan is done
- Plan-aware re-entry footer: precise step-tracking context replaces open-ended "call another tool" prompt

**Slice 2**: Plan gate (discuss before fire)
- Model emits `kind: "plan_proposal"` → parks in `parked_plan_turn` → operator confirms → turn resumes with `plan_confirmed = true`
- `TurnPhase::PlanningDiscussion`, `AgentAction::PlanProposal`, `plan_confirmed`/`plan_confirm_note` on `WorkingTurn`

**Slice 3**: Conditional preapproval (earned standing permission)
- `approval.request_standing { tool_name, required_successes }` — model registers a streak threshold
- After N consecutive successes, tool auto-added to `preapproved_tools`; resets on failure; persists via checkpoint
- `tool_success_streak` + `pending_preapproval_thresholds` on `SessionState`

**Why:** Root cause of Beacon's spin loop was biased re-entry prompt + no dedup + no discuss-before-fire gate.
**How to apply:** When debugging future spin loops, check `working_tool_history` for repeated (tool, args) pairs and `active_plan` step statuses.
