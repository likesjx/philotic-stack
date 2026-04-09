---
title: Native Overlay / VPN Proposal
doc_type: historical
domain: mesh-placement
status: historical
last_updated: 2026-04-08
tags:
- archived
- proposal
- networking
related_docs:
- ARCHITECTURE_STATUS.md
---

# Native Overlay / VPN Proposal

## Goal

Define what it would take for Philotic hotels to provide their own secure overlay network so current inter-hotel routing decisions can migrate off transitional host-level VPN scaffolding without a transport rewrite.

## Core Recommendation

- Separate the network into:
  - a **control plane** for gossip, liveness, capability advertisement, and lightweight coordination
  - a **data plane** for routed hotel-to-hotel execution traffic
- keep the native overlay/VPN implementation as its own process rather than burying all network substrate concerns inside arbitrary hotels
- Keep UDP for the control plane.
- Move routed execution off raw UDP datagrams and onto negotiated point-to-point transports owned by hotel towers.
- Treat host VPNs such as Tailscale/WireGuard as transitional underlay, not as the long-term identity or transport contract.
- Make application-layer identity, authorization, and transport negotiation independent of the current underlay so a Philotic-native overlay can replace it incrementally.

## Disposition

`implemented`

## Current Slice

This slice captures the architecture Philotic should preserve if it eventually implements its own overlay/VPN and proves the first point-to-point execution step.

Accepted in this slice:
- control-plane gossip and execution-plane task traffic are different concerns and should not share the same transport assumptions
- UDP remains appropriate for heartbeat, capability advertisement, pulse, and small coordination messages
- routed model/tool/task execution should move to point-to-point reliable channels selected by towers
- tower transport selection must stay under a stable routing contract so current Tailscale/WireGuard deployment can later migrate to a native overlay without schema breakage
- hotel or node identity must not be defined by IP/port
- the first point-to-point execution transport now exists as a TCP execution plane for routed inter-hotel task traffic
- the first honest two-hotel remote model smoke is now green over that TCP execution plane in local development
- hotels now advertise node-level execution reachability (`protocol`, `host`, `port`) in heartbeat/registry state for that first execution plane

Reality that triggered and then validated this proposal:
- the first honest two-hotel remote model smoke reached real remote route selection, then failed because the old UDP execution path hit `Message too long (os error 40)` for model payloads
- the replacement TCP execution plane now carries routed model traffic successfully in the local two-hotel smoke, which is strong evidence that Philotic's transport split is directionally correct

Linked work surface: [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)

## Identity vs Reachability

Hotels need stable identity that survives network changes.

Carry separately:

- `hotel_id` as the stable operator-facing authority label
- `node_id` as the runtime transport identity
- transport reachability records such as host, port, protocol, relay hints, and public key material

Do **not** treat current IP or DNS address as canonical identity.

This keeps Philotic migratable across:

- Tailscale / WireGuard underlay
- direct host networking
- NATed local networks
- future Philotic-native overlay transport

## Control Plane

The control plane is for lightweight shared state.

Allowed responsibilities:

- heartbeats and liveness
- capability and incarnation advertisements
- placement hints
- health summaries
- routing metadata
- compact acknowledgments and coordination
- reachability advertisements for execution-plane transports

Desired properties:

- low overhead
- tolerant of occasional loss
- fast convergence
- small bounded message sizes

UDP remains a good fit here.

## Data Plane

The data plane is for actual execution traffic once routing resolves a destination.

Allowed responsibilities:

- task invocation
- task result delivery
- streaming progress and partial output
- reliable request/response correlation
- large payload references and artifact exchange
- backpressure-aware delivery

Desired properties:

- reliable transport
- framed multiplexing or correlation
- streaming support
- large payload support
- resumable or replay-aware behavior where appropriate

Raw UDP is not a good final fit here.

## Tower Responsibility

Towers should own execution transport selection once a route is resolved.

Routing decides:

- which hotel
- which role/capability
- which incarnation if pinned

The tower decides:

- which point-to-point protocol to use
- whether direct or relayed connectivity is required
- whether the payload can ride inline or must use blob/off-band transfer

This allows one routed task contract to use different execution protocols over time:

- local socket
- TCP stream
- QUIC
- WebRTC data channel
- future overlay-native stream

## Security Requirements

If Philotic ever implements its own overlay/VPN, application-layer trust still needs to stand on its own.

Required principles:

- mutual hotel authentication independent of transport
- signed or authenticated capability advertisements
- execution-plane authorization at the hotel boundary
- replay resistance for control-plane signals
- transport encryption for data-plane channels

The VPN or overlay should be a network substrate, not the sole trust boundary.

## Reachability Advertisement

Hotels should eventually advertise not just capability, but how they can be reached for execution.

Execution-plane reachability records should be able to carry:

- supported protocols
- preferred protocol order
- control-plane address
- execution-plane addresses
- relay requirement or preference
- certificate fingerprint / public key / identity material
- bandwidth or streaming hints when relevant

That lets today's host VPN underlay and tomorrow's Philotic-native overlay use the same routing contract.

## Migration Strategy

Philotic should migrate in stages rather than attempting a full VPN rewrite up front.

1. Keep Tailscale/WireGuard as transitional underlay.
2. Restrict UDP mesh to control-plane traffic and small coordination messages.
3. Add the first point-to-point execution transport for routed tasks.
4. Add execution-plane reachability advertisement to the control plane.
5. Make towers negotiate among available execution transports.
6. Add relay and overlay capabilities only after direct point-to-point execution is proven.
7. Replace the host VPN underlay incrementally once the native overlay has equivalent trust, reachability, and operator ergonomics.

Current implementation note:
- step 3 now exists as the first TCP execution plane for routed `MESH_EVENT_BATCH` delivery
- step 4 has begun with node-level execution reachability advertisement in heartbeat/registry state
- steps 5-7 remain future work

## What Building The Actual VPN Would Require

Philotic would eventually need at least:

- cryptographic node identity and key lifecycle
- peer discovery and authenticated reachability exchange
- secure control-plane gossip
- secure data-plane connection establishment
- NAT traversal and relay strategy
- packet framing or stream multiplexing
- encrypted tunnel or stream semantics
- policy-aware routing and authorization
- rotation, revocation, and recovery workflows
- operator tooling for bootstrap and debugging

That is achievable, but it is a real networking/control-plane project, not a weekend rename of the current UDP sender.

## Recommended First Technical Slice

Do **not** start by building a full VPN tunnel.

Start by:

1. formalizing control-plane vs data-plane separation
2. adding a point-to-point hotel execution channel for routed tasks
3. keeping blob transport for large artifacts
4. teaching towers to advertise and negotiate execution transports

That gives Philotic the correct transport boundary now and preserves a clean path to a future native overlay.
