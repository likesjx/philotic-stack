---
title: Scripted Hotel Sensors
doc_type: proposal
domain: runtime-sessions
status: accepted-current-slice
last_updated: 2026-10-19
tags: [sensors, cron, lifegraph]
related_docs:
- ARCHITECTURE_STATUS.md
task_refs:
- docs/task.md#scripted-hotel-sensors
proposal_id: scripted-hotel-sensors
active_seams:
- scripted-sensor-consolidation
source_of_truth_targets:
- crates/data-memorygraphrag/src/sensor_scripts.rs
- crates/data-memorygraphrag/src/sensor_provider.rs
- crates/aiua/src/service/cron_ticker.rs
---

# Scripted Hotel Sensors

## Disposition

Local consolidation candidate from `codex/scripted-hotel-sensors` (`a6814059`)
and `codex/automation-deterministic-first` (`cc713582`). Their engine and provider
sources are identical; carry that implementation once against current develop.
Original branches remain preserved. Production activation is outside this slice.

## Dormant implementation

Both aiua and data-memorygraphrag use a nondefault `scripted-sensors` feature.
Default builds retain existing heartbeat and cron behavior. When explicitly
compiled in, cron passes through only operator-owned `sensor.run` tasks addressed
to exactly `life-graph-runner`; other payloads retain existing wrapping and policy.
No scripts, jobs, configuration, services or deployment are installed here.

The provider rejects missing, disabled and unapproved definitions. A delivery
attempts to persist `delivery_pending` before dispatch, then delivered or failed.
These saves are best effort: failures only warn and do not prevent delivery.
Dispatch refusal returns a successful provider response with `delivery_failed`.
This is not durable or exactly-once dispatch. `investigate` is recorded but has
no execution path in this slice.

## Authority and limits

The stored `operator_approved` boolean is not authenticated approval. Host callbacks
can invoke the full LifeGraph provider, read configuration and deliver to roles.
Rhai operation, recursion, expression, string, array and map limits bound script
execution, but do not bound host callback wall time or restrict callback authority.
Before activation, separately review authenticated approval, read/write boundaries,
configuration access, delivery destinations and callback deadlines. Do not enable
the feature in release or deployment configuration as a consolidation side effect.

## Verification

Local tests exercise engine verdicts, callback errors and operation-budget exhaustion,
plus injected provider I/O and cron routing. They do not establish live hotel
readiness, production authority safety or deployment acceptance.

Passed locally: seven engine tests, three injected provider tests, 26 cron tests
with the feature enabled and the same 26 with default features, plus all 228 default
data-memorygraphrag library tests. Independent review approved the dormant scope.
