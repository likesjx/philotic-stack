name: sver-harness
description: Use this skill for the mechanics of harness trial tracking and close-out in Philotic. It owns trial start/report/close discipline, explicit verification on completed closes, and the narrow process glue around SVER telemetry. Use it for harness lifecycle mechanics, not for deciding verification level or advancing the ladder.
domain: runtime-sessions
sver: verification-ladder-proposal

# SVER Harness Workflow

Scope: harness trial mechanics, session telemetry, and close-out hygiene.

This skill owns the operator-facing mechanics that keep SVER honest:

- start a measured trial
- report meaningful activity
- refuse empty/no-op telemetry
- require explicit verification when a completed trial closes
- keep harness close-out aligned with the verification ladder

This skill does not own:

- deciding which verification rung evidence supports
- advancing proposal/seam verification state
- proposal disposition changes

Use [$verification-orchestrator](../verification-orchestrator/SKILL.md) for ladder decisions and advancement.
Use [$session-hygiene](../session-hygiene/SKILL.md) for stale-session cleanup and claim conflicts.
Use [$philotic-slice-closeout](../philotic-slice-closeout/SKILL.md) when the main task is ending an implementation slice.

## When to Use

- starting a harness trial for focused work on a seam
- recording telemetry after meaningful edits or tests
- closing a trial with evidence and a verification level
- checking whether a close path can bypass verification by accident

## Harness Trial Contract

### Start

Use `just harness-trial-start <seam-id> [harness] [profile]`.

Starting a trial should:

- claim the active session in the graph
- attach the session to the seam
- initialize a measured trial boundary

### Report

Use `just harness-trial-report <activity-type> [phase] [tokens_in] [tokens_out] [elapsed_ms] [lines_changed] [files] [note]`.

Reporting should:

- include at least one signal
- update phase and telemetry honestly
- accumulate file counts, line counts, tokens, and elapsed time when present
- reject empty reports instead of inventing motion

### Close

Use `just harness-trial-close [status] [verified] [summary]`.

Closing should:

- require explicit verification when `status=completed`
- preserve the reported telemetry on the session and seam
- close the workstream in the graph
- leave a durable summary for the next agent

## Operating Rules

1. Do not let defaults impersonate evidence.
2. Do not close a completed trial without explicit verification.
3. Do not report a trial activity with no actual signal.
4. Keep the mechanics in the harness skill and the judgment in `verification-orchestrator`.

## Suggested Profiles

- `philotic-operator` if the work is mostly harness management and slice close-out
- `philotic-verifier` if the work is mostly evidence review and verification advancement

## Output Expectations

Report:

- what telemetry was recorded
- what verification level was supplied
- whether the trial is still open
- whether the workstream close path was exercised
