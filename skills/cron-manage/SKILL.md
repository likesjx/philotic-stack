---
name: cron-manage
description: Create, inspect, enable, disable, and remove scheduled cron jobs on the hotel. Use when the operator or an autonomous workflow needs recurring work at a fixed schedule.
catalog:
  skill_name: cron.manage
  implied_tools:
    - cron.register
    - cron.list
    - cron.enable
    - cron.disable
    - cron.remove
    - session.status
  validation_state: validated
  skill_markers:
    - governed
    - high_agency
  field_sources:
    required_fields:
      - schedule
      - target_role
      - payload
    optional_fields:
      - job_id
      - guaranteed
      - paracrine_heartbeat_template
    repo_skill_path: skills/cron-manage/SKILL.md
    workflow: "cron.list → compose job → cron.register → verify"
---

# Cron Management

Use this skill to set up recurring scheduled work on the hotel. Cron jobs fire on a 7-field cron schedule, deliver a JSON payload to a target role's inbox, and survive hotel restarts.

## CronJob fields

| Field | Required | Notes |
|---|---|---|
| `schedule` | Yes | 7-field cron: `<sec> <min> <hour> <dom> <month> <dow> <year>`. Example: `"0 0 8 * * * *"` = 08:00 daily. |
| `target_role` | Yes | Role name whose inbox receives the trigger payload. |
| `payload` | Yes | JSON string delivered to the role. Supports `{timestamp}`, `{iso_timestamp}`, `{job_id}`, `{node_id}`, `{target_role}` interpolation. |
| `guaranteed` | No | Mesh-coordinated delivery (Slice 2+, ignored in current runtime). Default false. |

## Workflow

1. Call `cron.list` to see existing jobs and confirm no duplicate exists for this schedule + role pair.
2. Compose the job:
   - Choose a `target_role` that is configured and ready to receive the trigger
   - Write the `payload` JSON — the role's inbox will receive it as a task message
   - Choose the schedule using the 7-field cron format
3. Call `cron.register` with the full `CronJob` object (hotel assigns the `id`).
4. Verify: call `cron.list` again to confirm the job appears with `enabled: true` and a `next_fire_at` in the future.
5. To pause a job: call `cron.disable` with the `job_id`.
6. To resume: call `cron.enable` with the `job_id`.
7. To permanently remove: call `cron.remove` with the `job_id`.

## Common schedule patterns

| Goal | Schedule |
|---|---|
| Every 5 minutes | `"0 */5 * * * * *"` |
| Daily at 08:00 | `"0 0 8 * * * *"` |
| Hourly | `"0 0 * * * * *"` |
| Every 30 seconds | `"*/30 * * * * * *"` |
| Weekdays at 09:00 | `"0 0 9 * * MON-FRI *"` |

## Payload design

The payload is what the target role receives as its task. A job for a `role:` target **must** carry its instruction in a `message` string — the hotel refuses a payload with no `message`/`content`/`paracrine_signal` (`CRON_PAYLOAD_UNDELIVERABLE`), because the agent would drop it on every fire. Add `chat_id` only when the reply (and any approval question) should go to that chat:

```json
{
  "message": "Review new training samples and flag any requiring correction. Fired {iso_timestamp} by job {job_id}.",
  "chat_id": "7898847424"
}
```

The role should be able to act on the message without additional context injection.

## Tools and approvals during a fire (operator-owned policy)

Each fire is a full turn with the target role's toolset. A job may also carry a **turn policy** — a tool allowlist, preapproved tools, and an approval mode — but **only the operator sets it** (`phil cron policy <job_id> ...` or `POST /api/cron/:id/policy`). The hotel refuses `cron.register` with a `policy` from an agent (`CRON_POLICY_OPERATOR_ONLY`), and **editing (re-registering) a job clears its policy** — the operator approved the old instruction, not the new one.

When a fire needs a tool that requires approval and the policy does not preapprove it:
- with a `chat_id` in the payload, the operator is asked there;
- with no chat (or a `deny` policy), the tool is refused immediately — finish without it and say in the reply which tool the operator could preapprove.

If a recurring job genuinely needs a gated tool, tell the operator the job id and the tool; do not try to work around the denial.

## Paracrine heartbeat payloads

For Life Graph heartbeat jobs, prefer the canonical template in `ansible_mesh_core::cron::ParacrineHeartbeatTemplate` instead of hand-writing the JSON. The template emits a top-level `paracrine_signal` object, so the hotel ticker dispatches `action = "paracrine_signal"` and the subscriber role observes the signal without entering the conversational model path.

Minimum shape after template interpolation:

```json
{
  "paracrine_signal": {
    "signal_id": "cron:{job_id}:{timestamp}",
    "signal_type": "open_loop_staleness",
    "scope": "personal",
    "source_hotel": "{node_id}",
    "source_node": "{node_id}",
    "target_role_type": "attention-steward",
    "subject_refs": ["lifegraph:open_loop"],
    "cadence": "daily",
    "priority": "medium",
    "observed_at": "{iso_timestamp}",
    "expires_at": null,
    "payload_summary": "Scan stale open loops and defer unless policy says to record.",
    "policy_tags": ["adhd-support", "re-entry"]
  },
  "payload_summary": "Scan stale open loops and defer unless policy says to record."
}
```

Use `target_role = "attention-steward"` for the first observe-only Life Graph subscriber.

## Guardrails

- Do not register jobs that fire more frequently than the task actually requires — prefer longer intervals. Every fire is a full model turn that costs money: a sub-hour "anything new?" poll is almost never worth it (two 15-minute polling jobs ran up a ~$138 model bill in October 2026)
- Do not register jobs to `orchestrator` role unless the operator has explicitly authorized recurring orchestrator tasks
- Always call `cron.list` before `cron.register` to prevent accidental duplicates
- Jobs targeting roles that are not currently materialized will queue until the role materializes — this is expected behavior, not an error
