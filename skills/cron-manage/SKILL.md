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

The payload is what the target role receives as its task. Write it as if you are handing work to that role directly:

```json
{
  "action": "scheduled_check",
  "job_id": "{job_id}",
  "fired_at": "{iso_timestamp}",
  "intent": "Review new training samples and flag any requiring correction."
}
```

The role should be able to act on the payload without additional context injection.

## Guardrails

- Do not register jobs that fire more frequently than the task actually requires — prefer longer intervals
- Do not register jobs to `orchestrator` role unless the operator has explicitly authorized recurring orchestrator tasks
- Always call `cron.list` before `cron.register` to prevent accidental duplicates
- Jobs targeting roles that are not currently materialized will queue until the role materializes — this is expected behavior, not an error
