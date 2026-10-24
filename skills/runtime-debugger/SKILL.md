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
2. Verify whether the failing observation came from source truth, local build truth, or installed/runtime truth.
3. Verify the ladder in order.
4. Inspect process, transport, registration, routing, and state layers separately.
5. Prefer direct evidence.
6. Classify the failure boundary.
7. Close with a reality-gap note.

## Heuristics

- A running PID is not proof of readiness.
- A green unit test is not proof of live registration.
- A rebuilt source binary is not proof of installed runtime rollout.
- If push and request/response share a connection, assume framing/correlation matters.
- Do language-specific debugging only after locating the failing runtime layer.
- Do not assume the failing component shares the caller's language, compiler, or transport.

## Installed Runtime Gate

When debugging a supervised or installed runtime:

- identify the target, resolved installed path/version/checksum, running process identity and effective configuration; tie observations to that runtime
- unchanged-service inspection: qualify the existing running artifact without replacement or restart
- configuration-only change: verify the supported reload and effective configuration; restart only when required by that change
- binary deployment: verify installation and process replacement/restart onto the intended artifact; the same path with new file contents is insufficient

Missing identity evidence limits the claim; local tests never prove deployment.

## Bounded Diagnosis

For each step, state one question, the competing hypotheses, the smallest authorized distinguishing check, and an outcome-to-next-action mapping before running it. Set a timeout and stop condition. After inconclusive evidence, revise the hypothesis or check; do not invent replacement, restart, grant, or credential prerequisites.

Preserve sanitized error category, failing layer, exit/status code, correlation ID when safe, and whether execution began. Strip tokens, personal payloads and secret-bearing URLs; do not collapse authentication, authorization rejection, transport timeout, protocol/schema, missing capability, and functional failure into “unavailable.” A timeout alone does not prove which layer failed.

Check the actual assembled operator entrypoint, including quoting, arguments, environment selection, pipeline exit propagation and response parsing. A passing helper test does not validate its wrapper. Use disposable inputs/stubs and realistic non-production failure fixtures before an authorized live check.

| Observation | Next action |
|---|---|
| Qualified unchanged process | Inspect the failing boundary without a rollout prerequisite |
| Authentication/authorization error | Identify the authorized correction or report the gate; no blind restart |
| Transport timeout | Distinguish target/transport from protocol readiness with a bounded check |
| Missing capability/schema mismatch | Compare actual protocol surface to the required contract |
| Inconclusive result | Record uncertainty and revise the hypothesis, not the permissions |
| Tool rejection before execution | Preserve rejection and scope; follow AGENTS approval handoff rules |

## Output Expectations

Report:

- the failing boundary
- the evidence used
- the most likely cause
- whether the result is proven or inferred
- the next fix or validation step
