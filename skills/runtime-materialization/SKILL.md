---
name: runtime-materialization
description: Use this skill when designing or debugging how services, runners, workers, or components are materialized, become ready, go dormant, wake, or are placed in specific environments. Trigger for lazy startup, readiness policy, environment placement, runner lifecycle, or materialization fallback design. Do not use this skill for generic live-stack triage unless lifecycle and placement policy are the main seam.
---

# Runtime Materialization

Scope: General.

Use this skill when the main problem is lifecycle and placement policy for executors that may not always be running.

This skill owns:

- lifecycle state modeling
- startup/wait/fallback policy
- wake/sleep/teardown policy
- environment placement constraints
- routing behavior around non-ready instances

This skill does not own:

- generic runtime triage across all layers
- proposal maintenance
- per-slice close-out

Use [$runtime-debugger](../runtime-debugger/SKILL.md) when the main task is diagnosing a live failure in the current stack.

## Workflow

1. Identify lifecycle states.
2. Identify placement constraints.
3. Define startup policy.
4. Define wake/sleep policy.
5. Define routing behavior around non-ready instances.
6. Verify with direct evidence.

## Heuristics

- A registered component is not necessarily runnable.
- A materialized component is not necessarily ready.
- A ready component is not necessarily healthy under load.
- Fallback rules should be explicit, not improvised by timeout.
- If environment placement matters, model it first-class instead of hiding it in routing code.

## Output Expectations

Report:

- lifecycle states
- placement rules
- startup/wait/fallback policy
- evidence gaps
- the next operational risk to close
