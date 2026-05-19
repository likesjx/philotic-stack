---
title: Multi-Hotel Component Distribution Proposal
doc_type: proposal
domain: mesh-placement
status: proposed
last_updated: 2026-03-31
tags:
- distribution
- routing
- mesh
- placement
- active-seam
related_docs:
- ARCHITECTURE_STATUS.md
- INTER_HOTEL_ROUTING_PROPOSAL.md
- HOTEL_PERIMETER_TRUST_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: multi-hotel-component-distribution
implements: []
implemented_by: []
active_seams:
- multi-hotel-route-consistency
- cross-host-distributed-validation
- remote-materialization-ceremony
- capacity-relief-placement
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Multi-Hotel Component Distribution Proposal

## Goal

Define how Philotic should support splitting one end-to-end user interaction across multiple hotels, such as membrane on one hotel, agent on another, model on another, and tool runner on another.

## Disposition

`proposed`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Core Recommendation

Philotic should support **distributed component placement** as a first-class runtime pattern, not just as an accidental consequence of remote model/tool routing.

The intended shape is:

- membrane may live on one hotel
- agent may live on another
- model capability may resolve to another
- tool runner capability may resolve to another

while the route contract, session ownership, and reply path remain coherent.

## Why This Matters

The “three-body problem” is really a four-body problem with attitude:

- external interface hotel
- cognition hotel
- model hotel
- tool/execution hotel

If Philotic can only handle remote model fallback, then it has not yet proven the architecture it is gesturing toward.

## Non-Negotiable Invariants

### 1. Session-owned membrane reply routing

The reply path must remain owned by the session’s membrane binding.

Remote placement must never opportunistically choose a different membrane just because it looks convenient.

### 2. Shared routing vocabulary

All distributed hops should keep using the same route contract:

- `target_node`
- `target_role`
- optional pinned `incarnation_id`
- optional placement hints

### 3. Structured turn ownership

The active turn must remain coherent even when execution fans out across hotels.

### 4. Durable hop boundaries

Each routed hop should have durable request/result correlation and auditable failure handling.

## Current Reality

Today, Philotic has proven:

- remote model routing for `text.generate` and `media.analyze`
- first remote tool fallback placement
- TCP execution plane for routed inter-hotel task traffic
- cross-machine WebRTC offer/answer plus data-channel `ping`/`pong` over mesh-backed signaling
- cross-host mesh payload plus WebRTC smoke between `bjork/local-telegram` and `jane-vps/beacon-test-hotel`
- cross-host mesh payload plus WebRTC smoke between `mbp-jane` and `jane-vps/beacon-test-hotel`

But a broader multi-hotel vertical slice is still open because:

- membrane is intentionally session-owned and not yet part of general remote placement
- broader routed component classes are not all using the same remote-capable path yet
- inter-hotel ACK truth is still transitional
- trust/perimeter policy is not closed enough for a serious cross-host split

That means the next placement seam is operational rather than mystical: making it routine to place agents on any hotel, hand work off across hotels, and deliberately concentrate membranes on a VPS boundary without pretending membrane ownership has already been generalized.

### Membranes On `jane-vps`

The strongest immediate placement posture is:

- keep Telegram-facing membranes concentrated on `jane-vps`
- let laptops (`mac-jane`, `mbp-jane`) remain cognition / operator / local-tool hotels
- route work, role handoff, and remote materialization across that shared mesh instead of duplicating every membrane on every laptop

Why this is attractive:

- one VPS boundary is easier to supervise than a swarm of roaming pollers
- Telegram credentials and edge trust posture become more boring, which is a compliment
- laptops may sleep, roam, or change networks without also becoming the membrane perimeter

What remains open:

- a formal “membrane home hotel” placement policy
- remote materialization for philotes and role incarnations behind a VPS-owned membrane
- durable approval / reply routing proof when ingress is VPS-local and cognition lands elsewhere

## Remote Materialization Ceremony

When a hotel needs a component that is not currently live or routeable, the platform should treat that as an explicit ceremony rather than a lucky timeout cascade.

Recommended flow:

1. the requesting hotel publishes a **materialization intent**
   - capability or component kind needed
   - reason (`route-demand`, `prewarm`, `failover`, `capacity-relief`)
   - requirements and placement hints
   - the parked work reference, not the full user payload broadcast to everyone
2. the control/placement plane determines the best target hotel by fitness
3. the requesting hotel sends a **targeted materialization request** to the winning hotel
4. the winning hotel decides locally whether it accepts the request and can materialize
5. the winning hotel materializes the component locally and supervises it locally
6. once the component is ready, the winning hotel publishes updated route/readiness metadata
7. the requesting hotel releases the parked work onto the updated route

Important boundary:

- intents may be mesh-visible
- the concrete materialization request should be point-to-point to the winner
- readiness publication should happen before the parked request is released

Otherwise the system stops being a placement plane and starts becoming a distributed rumor mill.

### Materialization intent

The first useful intent shape is:

```json
{
  "intent_type": "remote_materialization",
  "requesting_hotel": "hotel-a",
  "component_kind": "philote",
  "required_capability": "agent.session.orchestrator",
  "reason": "route-demand",
  "preferred_hotels": ["hotel-b", "hotel-c"],
  "preferred_environment": "gpu",
  "parked_work_ref": "turn:session-123:turn-44",
  "requested_at": 1741810200
}
```

This is intentionally a coordination record, not a full process spec serialized into UDP gossip.

### Materialization request

Once the winning target is chosen, the requesting hotel sends a targeted request to that hotel with the concrete requirements needed for local startup:

- component kind / target role
- environment constraints
- configuration or artifact references
- optional required lease scope
- retention hint or idle policy
- correlation id back to the parked work

The target hotel still owns:

- actual process spawn
- local policy and admission checks
- local supervision
- local readiness reporting

The requester does not get to spawn remote processes by sheer force of desire, which feels like a healthy boundary.

## Capacity-Relief Placement

One important variant is when a hotel is overloaded and wants help rather than when a single parked request is missing a destination.

Recommended flow:

1. stressed hotel publishes a **capacity-relief signal**
2. candidate hotels publish offers or eligibility
3. placement scoring chooses a winning target
4. stressed hotel sends a targeted materialization request to that winner
5. winner materializes and publishes readiness
6. routing begins sending new work to the new target
7. old capacity drains and retires according to policy

The important operational rule is:

- this should prefer **drain and retire**
- not immediate panic kill

Otherwise “scale-out” becomes a very energetic synonym for “drop work while moving it.”

## Relationship To Leases

Remote materialization and lease authority are related but not identical.

- some components are routeable once ready and do not need an authority lease
- some components, such as agents or Telegram pollers, are `singleton-scoped` and need an authority lease before they may act
- a target hotel may materialize a standby candidate without yet granting acting authority

This means:

- materialization creates a candidate
- readiness makes it routeable
- lease authority, when required, makes it allowed to act

That separation is the only reason the system can support both replicated capacity and custody-bearing runtimes without constantly confusing itself.

## Recommended Validation Ladder

### Stage 1

Local multi-hotel:

- membrane on hotel A
- agent on hotel B
- model on hotel C

### Stage 2

Local four-hotel:

- membrane on hotel A
- agent on hotel B
- model on hotel C
- tool runner on hotel D

### Stage 3

Cross-host:

- at least one hop off the local machine
- explicit perimeter/trust policy enabled

## First Slice Recommendation

Before attempting the full distributed split:

1. extend remote-capable route metadata across remaining routed component classes
2. move ACK behavior toward strict post-commit truth
3. finish the perimeter/trust model for cross-host joins
4. define the first watched multi-hotel validation script for the membrane/agent/model/tool split
