---
name: verification-ladder
description: Use this skill when deciding how deeply to validate a change or when explaining confidence in a code change. Trigger for test planning, selecting the right validation rung, or classifying confidence as test-green, smoke-green, or watched-live-green. Do not use this skill for runtime root-cause investigation or Philotic slice close-out unless validation choice is the main task.
domain: runtime-sessions
sver: verification-ladder-proposal
---

# Verification Ladder

**SVER Source:** [VERIFICATION_LADDER_PROPOSAL](../../docs/architecture/VERIFICATION_LADDER_PROPOSAL.md)

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

## SVER State Machine

Defined in [VERIFICATION_LADDER_PROPOSAL](../../docs/architecture/VERIFICATION_LADDER_PROPOSAL.md):

```
PROPOSED → CODE-COMPLETE → TEST-GREEN → SMOKE-GREEN → UAT-GREEN → IMPLEMENTED
```

| State | Definition | Evidence Required |
|-------|-----------|-------------------|
| PROPOSED | Design approved, active seams defined | Proposal document |
| CODE-COMPLETE | Slice landed, all code committed | Git commits, PR merged |
| TEST-GREEN | Unit tests pass, coverage thresholds met | Test results, coverage report |
| SMOKE-GREEN | Integration verified, live validation passes | Smoke test logs |
| UAT-GREEN | Production validation complete | UAT sign-off |
| IMPLEMENTED | Architecture doc updated, full traceability | Graph shows complete chain |

## Ladder (Validation Depth)

1. Crate or unit tests
2. Integration or e2e tests
3. Binary smokes
4. Watched live run

## Decision Rules

Climb the ladder when the change affects IPC, networking, concurrency, supervision, routing, delivery, materialization, or environment-specific behavior.

Before claiming `smoke-green` or `watched-live-green` on an installed stack, confirm the installed runtime truth gate:

- target, resolved artifact identity, running process identity and effective configuration match the observation
- unchanged-service inspection qualifies the current running artifact without replacement or restart
- configuration-only reload proves the intended effective configuration; restart only when required
- binary deployment proves the installed artifact and process replacement/restart onto it; a stale running process fails the gate even if its path now contains the new binary

Tests never imply deployment. Report the actual environment and evidence; isolated smokes are not installed-stack acceptance. Use runtime-debugger's bounded diagnosis and assembled-entrypoint checks. Release status is separate from the historical SVER labels above: report implemented, tested, integrated, deployed and live-verified individually using philotic-slice-closeout.

## Output Expectations

Report:

- which rungs were run
- what passed
- what remains unverified
- the highest honest confidence level
- current SVER state and evidence links
