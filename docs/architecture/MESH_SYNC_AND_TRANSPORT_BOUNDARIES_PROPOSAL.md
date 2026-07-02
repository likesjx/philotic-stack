---
title: Mesh Sync And Transport Boundaries Proposal
doc_type: proposal
domain: membrane-transport
status: accepted-current-slice
last_updated: 2026-05-10
tags:
- mesh
- transport
- udp
- webrtc
- execution-plane
- graph-sync
related_docs:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
- INTER_HOTEL_ROUTING_PROPOSAL.md
- MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md
- HOTEL_USER_IDENTITY_AND_OPERATOR_AUTH_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: mesh-sync-and-transport-boundaries
active_seams:
- mesh-peer-reachability
- transport-boundary-clarity
- multi-hotel-runtime-rollout
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Mesh Sync And Transport Boundaries Proposal

## Goal

State the mesh transport contract plainly enough that operators, desktop surfaces, and future runtime slices stop telling three different stories about the same packets.

## Core Recommendation

- Treat UDP as the mesh state-sync substrate only.
- Do not use UDP as the long-term transport for routed execution traffic.
- Do not use UDP as the long-term transport for peer-to-peer payload traffic.
- Treat WebRTC as an optional point-to-point session transport after signaling, not as the transport for mesh state sync.
- Treat the mesh-shared graph as a projected shared state set, not as blind full-graph replication.

In one sentence:

`UDP syncs state; routed work uses reliable point-to-point transport; peer session traffic uses WebRTC when appropriate; canonical local graph ownership stays hotel-local.`

## Disposition

`accepted for current slice`

## Current Slice

This slice clarifies the intended boundary and names the remaining transitional truth honestly.

Accepted in this slice:

- UDP is the control/state-sync plane.
- Graph convergence should happen through explicit projected record classes, not full database replication.
- Routed task/data traffic should not be normalized onto UDP.
- durable execution event batches now have explicit execution-plane message types, and their acknowledgments return over the reliable execution transport instead of bouncing back onto UDP
- P2P interactive/media/data sessions should use WebRTC when that transport is actually required.
- Desktop/operator surfaces should describe peer freshness and sync state in those terms.

Still transitional in current code:

- some mesh coordination envelopes still share the same beacon transport family as heartbeat traffic
- WebRTC signaling currently rides the mesh envelope plane before a direct peer session exists
- the durable execution plane is still a mix of explicit point-to-point execution transport and older mesh event/control flows

## The Boundary

### UDP Control / State-Sync Plane

UDP should carry:

- heartbeats
- capability sync
- membership sync
- catalog sync
- freshness/liveness state
- compact reachability observations
- lightweight signed coordination messages

UDP should not be the normative plane for:

- routed task invocation payloads
- routed task result payloads
- large attachments or blob transfer
- desktop operator chat payload streams
- long-lived peer session data channels

### Reliable Routed Execution Plane

Once a destination hotel or guest is chosen, routed work should move to a reliable point-to-point transport.

That plane should carry:

- task invoke envelopes
- task results
- approval/control replies tied to routed work
- larger payload references and artifact flow

Current repo truth already points this way:

- [INTER_HOTEL_ROUTING_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/INTER_HOTEL_ROUTING_PROPOSAL.md) explicitly separates control-plane gossip from the execution plane
- current `aiua` execution reachability already advertises `protocol`, `host`, and `port`

### WebRTC Peer Session Plane

WebRTC should be treated as an optional session transport for:

- live peer-to-peer interactive channels
- low-latency bidirectional session traffic
- media/data sessions where direct peer connectivity is worth the ceremony

WebRTC is not:

- the graph sync mechanism
- the hotel membership mechanism
- the canonical control plane

It begins after signaling, not before.

## Mesh State Sync Scope

The mesh does not need or want naive full-graph replication.

What should sync mesh-wide:

- hotel membership and peer identity records
- canonical catalog records
- toolset / skillset profile records
- routing metadata
- capability advertisements
- role placement metadata when it affects cross-hotel routing
- operator-visible shared intelligence needed for routing, decisions, and self-improvement

What should remain hotel-local:

- local secrets and vault contents
- raw device-bound resources
- ephemeral runtime/process guts
- local working state that is not coordination-visible
- hotel-local authority records that are not part of the shared mesh projection

This is the right organism split:

- shared nervous system
- local body state

## Current Observed Repo Truth

Observed code today shows the following transport shapes:

- `Heartbeat` and `CapabilitySync` live in the beacon/heartbeat plane
  - [crates/ansible-mesh-core/src/heartbeat.rs](/Users/jaredlikes/code/philotic-stack/crates/ansible-mesh-core/src/heartbeat.rs)
  - [crates/ansible-mesh-core/src/beacon.rs](/Users/jaredlikes/code/philotic-stack/crates/ansible-mesh-core/src/beacon.rs)
- mesh registry freshness is driven by inbound heartbeat observations
  - [crates/ansible-mesh-core/src/registry.rs](/Users/jaredlikes/code/philotic-stack/crates/ansible-mesh-core/src/registry.rs)
- `aiua` still resolves heartbeat targets from hotel graph records, which is why stale peer reachability can poison liveness until observed endpoints repair it
  - [crates/aiua/src/main.rs](/Users/jaredlikes/code/philotic-stack/crates/aiua/src/main.rs)
- WebRTC signaling envelopes exist as `WebRtcSignalMessage` / `SignalPayload`
  - [crates/ansible-mesh-core/src/webrtc.rs](/Users/jaredlikes/code/philotic-stack/crates/ansible-mesh-core/src/webrtc.rs)
- hotel-side WebRTC lives in `webrtc_guest`
  - [crates/aiua/src/service/webrtc_guest.rs](/Users/jaredlikes/code/philotic-stack/crates/aiua/src/service/webrtc_guest.rs)
- routed execution already has a dedicated dispatcher / execution transport shape
  - [crates/aiua/src/service/mesh_dispatcher.rs](/Users/jaredlikes/code/philotic-stack/crates/aiua/src/service/mesh_dispatcher.rs)

That means the current code is already halfway to the intended split. The remaining problem is not lack of abstraction, but transitional bleed-through and rollout drift.

## Policy Statement For Future Slices

When adding or reviewing mesh traffic:

1. ask whether this is state sync, routed execution, or peer session traffic
2. put state sync on UDP only if it remains compact and coordination-shaped
3. put routed execution on a reliable point-to-point plane
4. put session/media/interactive P2P traffic on WebRTC when appropriate
5. do not let “it was easy to put in the beacon envelope” become architecture

That last one is important because convenience has a long and distinguished history of impersonating design.

## Next Seams

- remove remaining transitional routed/data traffic that still leans on the beacon family when it should be on the execution plane
- make peer reachability self-heal from observed signed traffic so roaming hotels reconnect without graph surgery
- classify the canonical mesh-shared graph projection explicitly in code instead of relying on operator intuition
- keep WebRTC signaling narrow and ensure it remains a session bootstrap path, not a second hidden control plane
