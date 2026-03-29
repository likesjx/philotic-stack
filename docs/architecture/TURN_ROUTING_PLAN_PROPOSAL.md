---
title: "Turn Routing Plan Proposal"
doc_type: proposal
domain: runtime-sessions
status: proposed
last_updated: 2026-03-26
tags:
  - routing
  - voice
  - streaming
  - model-controller
  - agent-loop
related_docs:
  - AGENT_LOOP_PROPOSAL.md
  - MODEL_CONTROLLER_PROPOSAL.md
  - MODEL_GRAPH_CATALOG_PROPOSAL.md
  - VOICE_MACHINE_PROPOSAL.md
  - ARCHITECTURE_STATUS.md
task_refs:
  - docs/task.md
proposal_id: turn-routing-plan
implements:
  - model-controller
  - agent-loop-gap-closure
implemented_by:
  - compiled-turn-routing-plan-slice
active_seams:
  - turn-routing-plan
  - staged-voice-routing
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
---

# Turn Routing Plan Proposal

## Goal

Define a first-class per-turn routing plan that lets Philotic choose the best
model/controller path for each stage of a user turn rather than pretending one
model choice owns the entire flow.

The motivating path is streaming voice:

1. ingest audio
2. transcribe or otherwise understand it
3. hand the result into the normal agent turn
4. run tools, skills, approvals, and memory as usual
5. synthesize the final reply back to audio for membrane delivery

## Core Recommendation

Compile a `TurnRoutingPlan` per inbound turn.

That plan should be stage-based, not model-based:

- `ingress`
- `cognition`
- `egress`

Each stage should carry:

- capability
- request class
- target role / node / incarnation
- context-envelope budget
- desired streaming posture
- provider/model/voice hints when relevant

The important boundary is:

- the session and agent own the turn
- the routing plan chooses stage-specific execution paths
- model controllers execute provider calls only

Do not let voice streaming become a second secret agent loop hidden inside the
audio path. That would be efficient only in the way a house fire is efficient at
producing heat.

## Disposition

`proposed`

## End-State Flow

For an inbound streaming voice turn:

1. `membrane` receives streaming audio and binds it to a session-owned turn
2. the agent compiles a routing plan
3. ingress stage routes to the best `voice.transcribe` or future `voice.dialogue` path
4. transcript or structured audio-understanding output re-enters the normal agent turn
5. cognition stage routes to the best text-capable model path
6. tools, skills, approvals, memory, and partial replies remain owned by the normal agent loop
7. egress stage routes the final text or spoken-text channel to `voice.synthesize` or future native `response.generate`
8. `membrane` streams the result back to the user

## Context Envelope Split

The routing plan should express stage-aware context envelopes instead of dumping
the whole turn context into every stage.

### Ingress envelope

- session identity
- stream or turn identity
- language or transport hints
- minimal recent user-turn context
- no tool projection

### Cognition envelope

- full conversational turn context
- role activation and rules
- tool and skill projection
- memory/context projection
- approval state

### Egress envelope

- final display text
- optional spoken-text channel
- voice/persona policy
- channel/media constraints
- no tool projection

## Current Slice

Define and prove the first compiled plan shape only.

This slice should:

- define `TurnRoutingPlan` and per-stage records
- compile the current voice turn into:
  - ingress transform stage
  - cognition stage
  - egress synthesis stage
- stay aligned with current runtime truth:
  - `voice.transcribe` is the current ingress transform capability
  - `text.generate` is the current cognition capability
  - `voice.synthesize` is the current egress capability
- mark streaming as a preferred stage property, not as proof that every stage already streams live

This slice should not yet:

- persist the compiled plan in checkpoints
- make the hotel or membrane consume the plan directly
- replace current capability routing calls with a new orchestration layer
- collapse the current `voice.transcribe` vs future `speech.transcribe` naming seam by wishful thinking alone

## Relationship To Model Graph

The model graph/catalog should inform the routing plan.

It can tell the planner things like:

- which models support native audio
- which endpoint families are live/session-based
- coarse tradeoffs between speed, thinking depth, and audio capability

But the graph is still static metadata.

The routing plan is where static catalog truth, session policy, and live route
availability finally meet in one place.

## Next Seams

- persist or surface compiled routing plans for observability
- let `model.manager.list@1` and future planner logic use catalog metadata together
- add native `voice.dialogue` / `response.generate` routes when the runtime path exists
- normalize the long-term capability vocabulary around audio ingress without lying about current code
