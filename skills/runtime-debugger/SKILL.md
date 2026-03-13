---
name: runtime-debugger
description: Use this skill when debugging live runtime behavior in a multi-process or distributed system, especially IPC framing, socket registration, supervised process startup, routing/materialization, liveness, or request/response mismatches. Trigger when the user asks to watch a live stack, debug runtime integration issues, inspect process/socket state, or compare test behavior against real execution. This skill is general and should not assume a Rust-only stack.
---

# Runtime Debugger

Scope: General.

Use this skill for live-stack debugging where tests are not enough.

This skill owns:

- locating the failing runtime boundary
- separating process liveness from readiness/registration
- distinguishing protocol, routing, bootstrap, and state-authority failures
- capturing runtime assumption-vs-reality gaps

This skill does not own:

- proposal maintenance
- per-slice commit/push closure
- materialization policy design unless debugging reveals that as the failing seam

Use [$runtime-materialization](../runtime-materialization/SKILL.md) when the main task is designing lifecycle or placement policy rather than debugging a live failure.

## Workflow

1. Identify the expected runtime shape.
2. Verify the ladder in order.
3. Inspect process, transport, registration, routing, and state layers separately.
4. Prefer direct evidence.
5. Classify the failure boundary.
6. Close with a reality-gap note.

## Heuristics

- A running PID is not proof of readiness.
- A green unit test is not proof of live registration.
- If push and request/response share a connection, assume framing/correlation matters.
- Do language-specific debugging only after locating the failing runtime layer.
- Do not assume the failing component shares the caller's language, compiler, or transport.

## Output Expectations

Report:

- the failing boundary
- the evidence used
- the most likely cause
- whether the result is proven or inferred
- the next fix or validation step
