---
title: "Mesh Visibility And State Placement Proposal"
doc_type: proposal
domain: mesh-placement
status: proposed
last_updated: 2026-03-12
tags:
  - mesh
  - replication
  - state
  - sqlite
  - active-seam
related_docs:
  - ARCHITECTURE_STATUS.md
  - MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md
  - TELEGRAM_POLL_LEASE_PROPOSAL.md
  - KEY_VAULT_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: mesh-visibility-state-placement
implements: []
implemented_by: []
active_seams:
  - mesh-visible-state-contract
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
---

# Mesh Visibility And State Placement Proposal

## Goal

Define how Philotic should decide:

- which truths remain hotel-local
- which truths must become mesh-visible
- which truths should stay single-writer with remote inspection or delegation
- when local SQLite or file-backed state is no longer the right authority boundary

## Core Recommendation

Philotic should default to **hotel-local canonical authority with explicit mesh-visible projections**, not wholesale cross-hotel replication of SQLite rows or ad hoc file sync.

The system needs one deliberate classification pass for state instead of letting every new coordination problem invent its own export ritual:

1. **Hotel-local only**
   - secrets
   - ephemeral working turn state
   - local caches
   - restart-local supervision details
2. **Hotel-owned canonical, remotely queried or delegated**
   - vault operations
   - agent-home authority
   - admin actions that must execute on one owning hotel
3. **Mesh-visible metadata**
   - ownership
   - health
   - state/version
   - routability and availability hints
   - lease owner identity
4. **Single-writer leased state with mesh-visible owner**
   - Telegram poll authority
   - other transport cursors or externally serialized ingress positions
5. **Replicated or federated state**
   - only when the data is naturally append-only, conflict-tolerant, or explicitly modeled for multi-writer semantics

The mesh should publish **records shaped for coordination**, not raw storage internals. SQLite is a useful local authority and a bad religion.

## Disposition

Proposed.

## Current Slice

Define the first shared decision framework for:

- identifying sync-worthy truths
- distinguishing local authority from mesh-visible metadata
- deciding when SQLite or file-db storage should remain local plumbing versus when a higher-level mesh/state-plane contract is needed
- giving existing active seams, especially Telegram poll lease and vault metadata, one shared vocabulary

Linked task surface: [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)

## Why This Matters

Several current seams are already pointing at the same missing boundary:

- Telegram poll lease wants canonical mesh-visible authority, not only hotel-local runtime truth.
- Key vault wants mesh-visible metadata while keeping secret material local and protected.
- Multi-hotel placement needs shared visibility into ownership, availability, and delegation.
- Future control-plane and perimeter work will need the same question answered for health, authority, and policy state.

If each seam solves this separately, Philotic will end up with five slightly different “authoritative record” patterns and a sixth document explaining why that was somehow intentional.

## State Classification Rule

Before making state mesh-visible, answer these questions:

1. What component is the canonical writer?
2. Who needs to read it outside that writer's hotel?
3. Is the remote need inspection, routing, delegation, or mutation?
4. Is stale visibility acceptable?
5. Is the state externally serialized already?
6. Does the state have multi-writer semantics, or should that be forbidden?

Recommended default:

- if one hotel should own mutation, keep mutation local
- if other hotels need awareness, publish a mesh-visible projection
- only replicate the canonical record itself when multi-writer or portable durability is truly required

## When State Should Become Mesh-Visible

Promote a truth into a mesh-visible record when at least one of these is true:

- another hotel must make a routing or placement decision from it
- another hotel must detect whether the owner is healthy or unavailable
- remote operators need safe inspection without shelling into the owning hotel
- the current seam keeps inventing ad hoc query paths for the same fact
- authority handoff, delegation, or failover depends on the wider mesh seeing one owner

Do not promote state just because it exists in SQLite today. Storage location is not architecture.

## When SQLite Or File-DB Is Becoming Too Cumbersome

SQLite or file-backed truth is the wrong boundary when one or more of these starts happening repeatedly:

- cross-hotel coordination depends on data trapped in one local file
- remote readers need structured freshness or ownership semantics
- multiple features need their own export/projection code from the same local tables
- local row shape is being treated as a network contract
- state needs explicit leasing, epochs, health, or delegation beyond one machine
- recovery or failover depends on non-local visibility rather than local restart only

Recommended interpretation:

- **SQLite is still fine** when the problem is local durability, local querying, and local single-writer authority
- **a mesh-visible metadata layer is needed** when other hotels need to inspect or route around that authority
- **a true state-plane or replicated log/service is needed** only when single-hotel ownership itself is the bottleneck

## Recommended Record Shape

Mesh-visible records should prefer a small common envelope:

```json
{
  "record_type": "telegram_poll_lease",
  "record_id": "telegram:bot-token-ref:sha256:abcd",
  "owning_hotel": "hotel-alpha",
  "canonical_writer": "ansible",
  "state": "active",
  "version": 7,
  "updated_at": 1741809600,
  "health_status": "healthy"
}
```

Then add type-specific fields only as needed.

Suggested common fields:

- `record_type`
- `record_id`
- `owning_hotel`
- `canonical_writer`
- `state`
- `version`
- `updated_at`
- optional `health_status`
- optional `lease_epoch`
- optional `delegated_from`

The point is not to force every record into identical shape. The point is to stop every seam from making up its own “small bit of mesh truth” format in isolation.

## First Candidate Record Families

Start classification with the state that is already asking for it:

- Telegram poll lease ownership and lease health
- vault metadata records
- agent-home authority and approved remote delegation
- routed capability availability that must be consumed outside the local hotel

Not all of these need the same replication strategy. They do need the same decision rubric.

## First Slice Recommendation

Before building a full state plane:

1. define the classification rubric and record envelope
2. classify current active candidates by storage/visibility tier
3. pick one state family to publish mesh-visible metadata for first
4. keep canonical mutation local unless the slice proves that local ownership itself is the real problem

My bias for the first proving ground is still Telegram poll lease plus vault metadata, because both are already teaching us the same lesson from opposite directions.
