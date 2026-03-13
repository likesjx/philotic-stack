---
title: "Hotel Perimeter Trust Proposal"
doc_type: proposal
domain: operator-control-plane
status: proposed
last_updated: 2026-03-12
tags:
  - perimeter
  - trust
  - identity
  - authorization
  - active-seam
related_docs:
  - ARCHITECTURE_STATUS.md
  - INTER_HOTEL_ROUTING_PROPOSAL.md
  - PERIMETER_EGRESS_CONTROL_PROPOSAL.md
  - MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: hotel-perimeter-trust
implements: []
implemented_by: []
active_seams:
  - hotel-membership-records
  - perimeter-authz-policy
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
---

# Hotel Perimeter Trust Proposal

## Goal

Define how Philotic hotels determine which peers are inside the trusted perimeter, how new hotels join that perimeter, and how bad actors are kept out once inter-hotel routing moves beyond local development.

## Disposition

`proposed`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Core Recommendation

Philotic should distinguish three different things that are currently too easy to blur together:

1. **discovery**
2. **identity**
3. **authorization**

A hotel should not be considered “inside the perimeter” merely because it can emit a heartbeat that looks plausible.

The perimeter should be defined by:

- a known hotel identity
- authenticated transport messages
- explicit membership / authorization policy
- revocation and rotation capability

## Current Reality

Today, Philotic has real but incomplete trust controls:

- Beacon control-plane packets can be HMAC-validated
- replay can be guarded by timestamp windows and nonce tracking
- peer hotels can be discovered from hotel records / inventory

But the perimeter is still transitional because:

- auth enforcement is still optional in dev
- the system is still largely PSK-shaped
- there is no first-class hotel join/invite lifecycle
- there is no crisp notion of “this hotel is trusted for these roles/capabilities, but not those”

That is enough for local development and controlled operator setups, but not enough to call the perimeter closed.

## Proposed Trust Layers

### 1. Discovery Layer

How hotels find each other.

Examples:

- rendered peer inventory
- heartbeat advertisements
- operator-provided hotel records

Discovery alone grants no trust.

### 2. Identity Layer

How a hotel proves who it is.

Recommended future shape:

- stable hotel identity
- node/runtime identity
- cryptographic key material
- signed or authenticated transport traffic

Current PSK/HMAC fits here only as a transitional mechanism.

### 3. Authorization Layer

What a known hotel is allowed to do.

Recommended first dimensions:

- allowed membership in the perimeter
- allowed capabilities / route classes
- allowed transport endpoints
- allowed relay / forwarding behavior

This is the difference between:

- “I know who you are”
- and
- “I trust you to join this mesh and receive this work”

## Join / Membership Recommendation

Philotic should eventually have a hotel join model rather than relying purely on shared config osmosis.

Recommended lifecycle:

1. invite or trust bootstrap
2. identity exchange
3. authorization grant
4. active membership
5. rotation / revocation

This should be explicit enough that an operator can answer:

- which hotels are in the perimeter
- why they are trusted
- what they are trusted to do
- how to remove them

External membranes should use the same trust grammar for outside principals and transport infrastructures:

- external agent peer identities
- relay inventories
- trust classes for external principals
- revocation and quarantine behavior

That does not mean an external `A2A` peer or `Nostr` relay becomes a hotel member. It means perimeter trust should not invent one vocabulary for hotel peers and another totally unrelated vocabulary for membrane-edge trust if the enforcement questions are structurally the same.

## Enforcement Recommendation

Inter-hotel comms should be able to reject traffic at multiple layers:

- bad or missing auth
- replay
- unknown hotel identity
- unauthorized hotel
- unauthorized capability/class
- stale or revoked membership

The rejection reason should be structured and auditable.

## First Slice Recommendation

Before pushing to broader cross-host routing:

1. define hotel membership records
2. define hotel identity/auth material
3. define authorization policy for perimeter membership
4. require authenticated control-plane traffic outside explicit dev mode
5. add revocation / deny behavior to the perimeter model
