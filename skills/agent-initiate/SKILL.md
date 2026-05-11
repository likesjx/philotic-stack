---
name: agent-initiate
description: Protocol for an agent to send a proactive, unsolicited message to a user or peer agent without waiting for a human trigger. Use when a cron job fires, a threshold is crossed, or a monitored condition is met.
catalog:
  skill_name: agent.initiate
  implied_tools:
    - session.status
    - hotel.status
    - delegate.whisper
  validation_state: validated
  skill_markers:
    - governed
    - high_agency
  field_sources:
    required_fields:
      - trigger_reason
      - recipient
      - message_intent
    optional_fields:
      - urgency
      - attach_context
    repo_skill_path: skills/agent-initiate/SKILL.md
    workflow: "verify trigger → compose message → route via delegate.whisper or membrane"
---

# Agent Initiate

Use this skill when you have a legitimate, non-speculative reason to reach out proactively — a cron job fired, a monitored table crossed a threshold, a long-running task completed, or a peer agent needs coordination.

Do NOT use this skill for:
- Unprompted opinion-sharing
- Repeating information the operator already has
- Sending a message "just in case"
- Any message where the trigger is not traceable to a concrete system event

## When initiation is legitimate

| Trigger | Legitimate? | Notes |
|---|---|---|
| Cron job fired with `{job_id}` payload | Yes | Job itself is the authorization |
| Table row count crossed operator-set threshold | Yes | Operator configured the threshold |
| Long-running subagent completed | Yes | Completion is the trigger |
| Peer agent sent a delegation request | Yes | Request is the trigger |
| No explicit trigger — "I thought of something" | No | Wait for the next operator turn |
| Speculative notification — "this might be useful" | No | Do not initiate |

## Workflow

1. Identify the trigger. If you cannot point to a specific system event (cron job ID, table threshold, task completion), stop — do not initiate.
2. Call `session.status` to confirm your current session and role context.
3. Compose the message:
   - State the trigger explicitly in the first sentence
   - Be concise — one paragraph maximum
   - Include any required action from the recipient, if any
4. Route the message:
   - **To a human operator via Telegram**: use `delegate.whisper` with the membrane gateway target role
   - **To a peer agent**: use `delegate.to_peer` with the target agent's role
5. Record the initiation event in your graph if it is part of an observable pipeline.

## Message structure

```
[AGENT NOTICE] <trigger in one sentence>

<What happened>: <brief factual summary>
<Context>: <why this matters now>
<Action required>: <what the recipient should do, or "no action required">
```

## Approval posture

Proactive outreach is **high-agency** — it surfaces content to humans without their request. Apply these constraints:

- Do not initiate more than once per trigger event
- Do not chain initiations (one trigger → one message)
- If approval_policy is `always_ask`, park the message as a pending action and surface it at the next operator turn instead of sending immediately
