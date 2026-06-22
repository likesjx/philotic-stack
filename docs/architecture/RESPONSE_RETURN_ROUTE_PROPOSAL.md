---
title: Response Return Route Proposal
doc_type: proposal
domain: mesh-placement
status: implemented
last_updated: 2026-06-22
tags:
  - routing
  - responses
  - tool-results
  - mesh
  - runtime-invariant
related_docs:
  - ARCHITECTURE_STATUS.md
  - INTER_HOTEL_ROUTING_PROPOSAL.md
  - MESH_SYNC_AND_TRANSPORT_BOUNDARIES_PROPOSAL.md
  - TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md
  - SESSION_LOOP_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: response-return-route
active_seams:
  - response-return-route
  - cross-hotel-tool-results
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
---

# Response Return Route Proposal

## Goal

Make response delivery exact.

Requests may be routed by role, session, policy, and placement. Responses must
return to the concrete guest that owns the in-flight work.

The live failure that exposed this: Jane's `life.recall` call reached the
`life-graph-runner` on `vps-jane`, but the datasource response returned to the
broad `agent` role on `mbp-jane` instead of the waiting `agent-jane` guest. Jane
remained in `WaitingTool` until the watchdog evicted the turn. Elegant in the
way a missing apartment number is elegant.

## Core Recommendation

Use this routing rule everywhere:

```text
Role routing is for requests.
Guest routing is required for responses.
```

For response-like payloads addressed to `target_role = "agent"`, the hotel must
resolve an explicit guest before delivery. That guest is the owner of the
pending turn, tool call, approval, paracrine exchange, or model call.

Response-like actions include:

- `model_response`
- `tool_result`
- `datasource_response`
- `paracrine_response`
- `approval_resolution`
- `handoff_bundle`

## Disposition

Implemented.

Implemented in this slice:

- `philote` includes `reply_guest_id` in `ToolExecutionPayload`.
- `datasource` echoes `reply_guest_id` into `EmitTask.target_guest_id` for both
  success and failure responses, falling back to `agent_id` for compatibility.
- `aiua` repairs response-like `agent` payloads that arrive without
  `target_guest_id` by inferring the concrete guest from:
  - embedded `delivery_target_guest_id`
  - embedded `reply_guest_id`
  - embedded `agent_id`
  - session `active_incarnation_id`
  - session `primary_agent_id`
- `life_graph_ipc_smoke_driver` can now exercise explicit target guest routing.

Completed in the follow-up implementation slice:

- added shared `philotic_client::ReturnRoute`.
- migrated `datasource`, `tool-runner`, `graph-runner`, and `model-router` to
  read and emit `ReturnRoute` while preserving compatibility fields.
- `philote` now emits `ReturnRoute` in `ToolExecutionPayload`.
- `aiua` rejects unrecoverable broad `agent` responses with
  `RESPONSE_ROUTE_UNRESOLVED` and pushes the failure into the heal queue when
  available.

Still pending:

- remove ad hoc `reply_to` / `reply_role` / `reply_guest_id` fields after all
  deployed components are confirmed to understand the
  shared typed `ReturnRoute` DTO.
- add first-class operator UI/status surfacing for response-route failures; today
  they are visible as structured IPC errors, logs, and heal queue entries.

## Current Slice

This proposal is implemented at the runtime boundary and remains in transitional
compatibility mode while older envelopes still carry flat reply fields.

Validation:

- local focused tests for agent response routing
- shared `ReturnRoute` compatibility tests
- `datasource`, `tool-runner`, `graph-runner`, `model-router`, `philote`, and
  `aiua` crate checks
- prior cross-hotel LifeGraph smoke from `mbp-jane` to `vps-jane`

## Return Route Contract

The durable DTO is:

```json
{
  "node": "mbp-jane-aiua-01",
  "role": "agent",
  "guest_id": "agent-jane",
  "session_id": "telegram:7898847424:agent-jane",
  "turn_id": "turn-lifegraph",
  "correlation_id": "optional"
}
```

During rollout, compatibility fields remain:

- `reply_to`
- `reply_role`
- `reply_guest_id`
- `session_id`
- `turn_id`

For final membrane egress, keep the separate transport reply route:

- `final_reply_to`
- `final_reply_role`
- `final_reply_guest_id`

Those routes answer different questions:

- return route: where should the runtime response re-enter cognition?
- final reply route: where should the user-visible answer be sent?

## Invariants

1. A response-like payload may never intentionally broadcast to every subscriber
   for role `agent`.
2. If a response-like payload has `reply_guest_id`, that guest wins.
3. If `reply_guest_id` is absent but `agent_id` names a concrete guest, the hotel
   may repair the route with that guest.
4. If neither is present, the hotel may infer from session active incarnation.
5. If inference is impossible, the hotel rejects the response with
   `RESPONSE_ROUTE_UNRESOLVED` instead of quietly parking or ledger-only
   dropping it.
6. Request routing remains session/role aware and may continue to choose active
   incarnations, persisted local delivery hints, or orchestrator fallback.

## Why This Belongs In Core

Every runner can forget a return field. The hotel cannot forget what kind of
message it is delivering.

Putting the invariant only in `life-graph-runner` would leave the same bug in:

- `tool-runner`
- `graph-runner`
- `model-router`
- remote model smoke paths
- future paracrine / heartbeat responders

The core hotel boundary is the last place where the runtime still has enough
context to repair or reject an unsafe response route.

## Next Seams

- `return-route-compat-removal`: remove flat reply fields once all live hotels
  are on `ReturnRoute`.
- `response-route-operator-ui`: show response route failures in hotel status and
  operator diagnostics, not only logs and heal queue.
