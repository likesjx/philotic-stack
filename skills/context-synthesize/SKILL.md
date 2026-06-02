---
name: context-synthesize
description: Restore session continuity at the start of a new conversation by pulling current state from the hotel graph, memory, and role list — then synthesizing a working mental model before acting.
catalog:
  skill_name: context.synthesize
  implied_tools:
    - session.status
    - hotel.status
    - role.list
    - skill.list
    - graph.query
  validation_state: validated
  skill_markers:
    - governed
    - self_directed
  field_sources:
    required_fields: []
    optional_fields:
      - prior_context_summary
      - focus_task
    repo_skill_path: skills/context-synthesize/SKILL.md
    workflow: "session.status → hotel.status → role.list → graph.query → synthesize"
---

# Context Synthesis

Use this skill at the start of any new session, after a context compaction, or when you suspect your mental model is stale. It rebuilds your working context from live system state rather than relying on in-memory assumptions.

Context synthesis is a **prerequisite for meaningful action** — do not start substantive work until you have a current picture of what is true in the system.

## When to run

- At the start of every new session (before any other action)
- After a context compaction (your prior conversation is gone)
- When tool results contradict your current assumptions
- When you have been handed off from another role and need to orient

## Workflow

1. **Call `session.status`** — confirm session ID, turn count, active approval state, active sessions.
2. **Call `hotel.status`** — get the live guest list and agent identities. This confirms which roles are materialized and what the actual hotel name is.
3. **Call `role.list`** — see your current role roster, toolset profiles, and home hotel assignments.
4. **Call `skill.list`** — confirm which skills are assigned to your active role.
5. **Optional: query the graph** — if you need to reconstruct task context, call `graph.query` with a targeted query (e.g., recent decisions, open proposals, active tables).
6. **Synthesize** — in your internal reasoning, form a brief working model:
   - What hotel am I on?
   - What roles do I have and what are their toolset profiles?
   - What work was in progress (from prior context summary, if available)?
   - What is the first action I should take?
7. **State your orientation** — before answering the operator or taking action, briefly surface your working model: "I'm oriented on hotel X as role Y. I see Z in flight."

## What to do when state is missing

If `hotel.status` returns empty guests or `role.list` returns no roles:
- Do NOT assume the system is broken
- Call `hotel.logs` to check for recent restart events
- Report the state to the operator and wait for guidance before acting

## Synthesis output

You do not need to call a tool to save the synthesis — it lives in your working context. But if you discover something non-obvious (e.g., a guest that failed to materialize, a role missing from the roster), record it with `graph.create` as a node so it survives the session.

## Relation to Muninn

If the hotel has Muninn memory configured, context.synthesize complements it:
- Muninn provides *past decisions and learned context* across sessions
- context.synthesize provides *current live system state* for this session
- Use both: recall from Muninn, then verify against live hotel state before acting on the recall
