---
name: session-recover
description: Diagnose and recover from a stuck, failed, or confused session state. Classifies the failure, chooses the minimal recovery path, and resumes work without operator intervention when possible.
catalog:
  skill_name: session.recover
  implied_tools:
    - session.status
    - hotel.status
    - hotel.logs
    - role.list
  validation_state: validated
  skill_markers:
    - governed
    - self_directed
  field_sources:
    required_fields:
      - failure_class
      - recovery_action
    optional_fields:
      - context_summary
      - escalate_to_operator
    repo_skill_path: skills/session-recover/SKILL.md
    workflow: "session.status → classify → choose path → recover or escalate"
---

# Session Recovery

Use this skill when you detect that the session is stuck, a tool has failed repeatedly, context has become corrupted, or you cannot make forward progress. Recover autonomously when the failure class permits; escalate explicitly when it does not.

## Failure Classification

Before acting, classify the failure:

| Class | Signs | Autonomous recovery? |
|---|---|---|
| **tool_not_found** | Tool returns "not in toolset" or permission denied | Escalate — request capability |
| **transient_ipc** | Single tool call timed out or got transport error | Retry once, then escalate |
| **context_drift** | Tool results contradict earlier state (phantom roles, missing guests) | Re-anchor via session.status + hotel.status |
| **loop_detected** | Same tool call issued 3+ times with same result | Stop loop, surface to operator |
| **approval_blocked** | Turn parked waiting for approval with no response | Surface status; do not retry |
| **model_confusion** | Repeated nonsensical tool sequences not matching the task | Handoff.back to orchestrator with context dump |

## Workflow

1. Call `session.status` — get turn count, active tool runners, approval state
2. Call `hotel.status` — verify guests are alive, roles are populated
3. If logs are needed, call `hotel.logs` with lines=100
4. Classify the failure using the table above
5. Take the minimal recovery action:
   - **transient_ipc**: retry the failed tool once
   - **context_drift**: rebuild mental model from fresh tool state; do not rely on prior assumptions
   - **loop_detected**: immediately stop; report the loop pattern to the operator
   - **tool_not_found**: use `capability.request` skill to request the missing tool from the orchestrator
   - **approval_blocked**: report status to the operator; do not attempt to bypass
   - **model_confusion**: call `handoff.back` with a clear context dump and request orchestrator review

## What NOT to do

- Do not retry a failed tool more than once without reclassifying
- Do not call `bash.exec` as a recovery shortcut for IPC failures
- Do not fabricate tool results to continue the session
- Do not silently drop a failure — always surface it, even if it means `handoff.back`

## Escalation format

When escalating to the operator or orchestrator, include:

```
RECOVERY ESCALATION
Failure class: <class>
Observed: <what happened>
Attempted: <recovery steps taken>
Status: <current session state>
Request: <what is needed to continue>
```
