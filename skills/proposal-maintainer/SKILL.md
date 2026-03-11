name: proposal-maintainer
description: HIGH PRIORITY. Use this skill to keep architecture and process proposals concise, current, and linked to active tasks. A proposal is not a static doc; it is an active plan. Trigger for disposition updates, task alignment, or removing stale certainty. Every meaningful decision should ripple into a proposal update.

# Proposal Maintainer

Scope: General.

Use this skill for proposal and spec hygiene across projects.

This skill owns:

- tightening proposal structure
- adding or updating `Disposition`
- adding links to active task/work-item tracking
- clarifying implemented vs deferred vs superseded status
- trimming stale or overly broad narrative

This skill does not own:

- per-slice commit/push workflow
- repo-specific task-board closure unless it directly supports proposal accuracy
- runtime debugging or validation decisions

Use `philotic-slice-closeout` when the main job is closing a Philotic implementation slice.
Use [$verification-ladder](../verification-ladder/SKILL.md) when the main job is choosing or reporting validation depth.

## Workflow

1. Identify the proposal's job.
2. Check whether it needs a `Disposition`, `Current Slice`, or task links.
3. Tighten the recommendation and remove stale certainty.
4. Align the document with implemented, accepted, deferred, or superseded reality.
5. Link to the authoritative work surface.

## Output Expectations

Report:

- what was clarified
- what status/disposition changed
- which work items or tasks are linked
- any stale sections still worth cleaning later
