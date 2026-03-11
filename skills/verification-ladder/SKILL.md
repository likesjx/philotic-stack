---
name: verification-ladder
description: Use this skill when deciding how deeply to validate a change or when explaining confidence in a code change. Trigger for test planning, selecting the right validation rung, or classifying confidence as test-green, smoke-green, or watched-live-green. Do not use this skill for runtime root-cause investigation or Philotic slice close-out unless validation choice is the main task.
---

# Verification Ladder

Scope: General.

Use this skill when the main question is: how much validation is required, or what confidence level is honest?

This skill owns:

- selecting validation depth
- mapping change type to test/integration/smoke/watched-run expectations
- describing confidence honestly

This skill does not own:

- runtime root-cause debugging
- proposal hygiene
- slice commit/push closure

Use [$runtime-debugger](../runtime-debugger/SKILL.md) when the issue is a live runtime failure.
Use `philotic-slice-closeout` when the main job is closing a Philotic implementation slice.

## Ladder

1. Crate or unit tests
2. Integration or e2e tests
3. Binary smokes
4. Watched live run

## Decision Rules

Climb the ladder when the change affects IPC, networking, concurrency, supervision, routing, delivery, materialization, or environment-specific behavior.

## Output Expectations

Report:

- which rungs were run
- what passed
- what remains unverified
- the highest honest confidence level
