---
name: verification-orchestrator
description: Coordinate the verification pipeline across proposals and seams. Track test coverage, manage SVER state transitions, and ensure verification claims match actual test results. Use when advancing proposals through the verification ladder.
---

# Verification Orchestrator

Scope: verification pipeline, SVER state, test-run tracking.

The verification ladder is the system's defense against false "done" claims. This skill orchestrates the ladder — ensuring that verification state transitions are backed by actual evidence.

## When to Run

- **After test runs**: Record results and check whether verification state should advance
- **Before claiming verification**: Validate that evidence exists
- **During proposal review**: Audit verification gaps across the pipeline
- **At check-engine time**: Surface proposals stuck in verification limbo

## Verification Ladder

The ladder has these levels (from lowest to highest):

| Level | Meaning | Evidence Required |
|---|---|---|
| `none` | No verification attempted | — |
| `code-complete` | Implementation done, not tested | Code review |
| `test-green` | Unit/integration tests pass | `just test-suite` green |
| `smoke-green` | Binary smoke tests pass | `just smoke-suite` green |
| `uat-green` | Full UAT including tier-2 | `just verify-vertical-slice` + tier-2 |
| `watched-live-green` | Confirmed in live runtime | Operator observation |

## Recording Test Results

### Via justfile (recommended)

```bash
just test-and-record proposal:agent-onboarding
```

### Via REST API

```bash
curl -s -X POST http://127.0.0.1:8900/api/test-run \
  -H "Content-Type: application/json" \
  -d '{
    "target_id": "proposal:agent-onboarding",
    "test_count": 27,
    "pass_count": 27,
    "fail_count": 0,
    "duration_ms": 5000
  }'
```

### Via MCP

```
graph_record_test_run({
  target_id: "proposal:agent-onboarding",
  test_count: 27,
  pass_count: 27,
  fail_count: 0,
  duration_ms: 5000
})
```

## Advancing Verification State

Only advance verification when the evidence supports it:

```
graph_advance_verification({
  target_id: "doc:desktop-membrane-proposal",
  from_level: "none",
  to_level: "test-green",
  evidence: "just test-suite passes, 27/27 tests green",
  agent: "cascade"
})
```

### Valid Transitions

- `none` → `code-complete` (code review done)
- `code-complete` → `test-green` (unit tests pass)
- `test-green` → `smoke-green` (smoke suite passes)
- `smoke-green` → `uat-green` (full UAT passes)
- `uat-green` → `watched-live-green` (operator confirms live)

Skipping levels is allowed only with explicit justification recorded in the decision.

### Invalid Claims

Do NOT advance to:
- `test-green` without a recorded passing test run
- `smoke-green` without smoke suite evidence
- `watched-live-green` without runtime truth gate checks (see WORKFLOW.md)

## Health Check

```bash
curl -s http://127.0.0.1:8900/api/health/proposals | jq .
```

This reports:
- Proposals with no verification at all
- Disposition distribution (how many proposed vs. accepted vs. implemented)
- Embedding coverage gaps

## Integration with Proposal Pipeline

When a test run is recorded with 100% pass rate against a proposal:
1. The test-run node is linked to the proposal via `tested_by` edge
2. Agents should check if this warrants a verification advancement
3. The advancement should be recorded via `graph_advance_verification`
4. The decision should be recorded via `graph_decide`

## Audit Checklist

At session end or during retrospectives:

1. Are there proposals at `test-green` that should be `smoke-green`?
2. Are there proposals at `code-complete` with passing test runs?
3. Are there proposals claiming verification that have no linked test-run nodes?
4. Are there proposals with `watched-live-green` that haven't been re-verified after changes?
