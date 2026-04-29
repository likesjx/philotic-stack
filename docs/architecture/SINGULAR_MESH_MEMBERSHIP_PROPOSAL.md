---
title: Singular Mesh Membership Proposal
doc_type: proposal
domain: mesh-placement
status: accepted for current slice
last_updated: 2026-04-29
tags:
- mesh
- membership
- trust
- routing
- webrtc
- role-transport
related_docs:
- MESH_PKI_HOTEL_IDENTITY_PROPOSAL.md
- MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md
- MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md
- ARCHITECTURE_STATUS.md
task_refs:
- docs/task.md
proposal_id: singular-mesh-membership
implements: []
implemented_by: []
active_seams:
- pairwise-mesh-membership-vs-global-convergence
- hotel-membership-replication
- mesh-wide-routing-view
- cross-hotel-role-transport
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Singular Mesh Membership Proposal

## Goal

Define Philotic mesh membership as one shared circle of trust rather than a loose pile
of bilateral ceremonies.

The intended operator model is simple:

- a mesh is singular
- any trusted hotel may invite a new outside hotel into that mesh
- once admitted, the new hotel becomes visible to all existing mesh members
- any member may route to any other member based on live reachability and policy
- a philote may transport itself, or one of its roles, across any reachable hotel in the mesh

Today the implementation behaves more like pairwise trust edges wearing a mesh costume.
This proposal names the gap and the target.

## Core Recommendation

Philotic should treat mesh membership as **globally converged membership with local
authority over acceptance**, not as permanently isolated inviter-specific trust edges.

Recommended model:

1. **Local ceremony, global consequence**
   - Any already-trusted hotel may issue an invite to an outside hotel.
   - The invite ceremony remains bilateral for security and audit.
   - A successful join adds the new hotel to the shared mesh membership set, not just to the inviter's personal address book.

2. **One mesh membership graph**
   - Every hotel in the mesh should converge on the same set of trusted hotel identities.
   - Membership records should replicate as mesh-visible authority metadata, not as ad hoc side effects of whichever hotel happened to do the invitation.

3. **Global routing view**
   - Every hotel should have a global view of:
     - trusted hotel identities
     - live mesh reachability
     - execution reachability
     - WebRTC signaling eligibility
     - role and capability routability metadata
   - Routing should then be a policy problem, not a “did I happen to join that exact inviter?” problem.

4. **Transport and role portability**
   - A philote should be able to:
     - hand work to any trusted hotel
     - transport itself to another hotel when policy allows
     - transport one or more active roles to another hotel
   - Role transport should use the same converged membership graph rather than a separate hidden trust system.

The mesh is the unit of trust. The inviter is the entry point, not the permanent owner of the relationship.

## Disposition

Accepted for the current slice.

Current truth is still transitional, but no longer purely pairwise:

- mesh invite/accept is real and secure
- payload routing and WebRTC can work across joined hotel pairs
- accepted membership now propagates mesh member records to the existing circle of trust and syncs the current circle back to the new member
- direct peer auth can now be derived from long-lived transport identities when a freshly learned member has not yet spoken directly
- revocation, richer audit, and full mesh-wide authority semantics are still open

This proposal now governs the first implementation slice toward converged singular membership rather than only naming the dream.

## Current Slice

This slice now does four things:

1. state the intended mesh model unambiguously
2. name the current implementation gap honestly
3. propagate accepted member records to the current mesh and sync the current mesh back to the newly accepted hotel
4. derive direct peer auth from stable transport identities when a pairwise cached key is missing

This is still transitional rather than “finished singular mesh.” The first converged membership path is implemented; revocation, richer audit lineage, and fully policy-driven role transport are not.

## Current Truth Vs Intended Truth

### Current truth

What is proven today:

- an existing hotel can invite an outside hotel
- the outside hotel can join via a secure invite ceremony
- paired hotels can exchange mesh payloads
- paired hotels can complete WebRTC when both runtimes are current

What is not yet true:

- one successful join does not automatically produce global membership convergence across every hotel
- hotels can still end up with pair-specific trust state
- some routing surfaces still behave as though trust is local to the inviter pair

### Intended truth

One successful accepted join should make the new hotel a member of the mesh as a whole.

That means every hotel in the mesh should eventually learn:

- the new hotel's stable identity
- its membership status
- its mesh reachability metadata
- its execution reachability metadata
- whether it is eligible for WebRTC signaling and direct role transport

If the mesh is singular in operator language, it should be singular in membership semantics too.

## Membership Model

### Membership record

Each hotel should converge on a shared mesh membership record for every trusted hotel:

```json
{
  "record_type": "mesh_member",
  "mesh_id": "default",
  "hotel_id": "beacon-test-hotel",
  "identity_pubkey": "<hotel identity key>",
  "membership_state": "active",
  "admitted_via": "mbp-jane",
  "admitted_at": 1777478400,
  "revoked_at": null,
  "routing_visibility": "global",
  "transport_modes": ["mesh", "webrtc"],
  "execution_reachability": {
    "protocol": "https",
    "host": "100.64.212.8",
    "port": 11924
  }
}
```

The inviter remains part of audit history. It should not remain the only hotel that knows the new member exists.

### Convergence rule

When Hotel A accepts Hotel D into mesh M:

- Hotel A records D as trusted
- Hotel D records A and mesh M as trusted entry context
- Hotel A emits a membership propagation event to all current mesh members
- other mesh members ingest that record, validate the signer/trust chain, and add D to their own mesh-visible membership set

This can be eventual rather than synchronous, but it must be authoritative and replayable.

## Routing Model

Once membership is converged, any member should be able to:

- send a mesh payload to any other member
- open a WebRTC signaling session to any other member
- ask the mesh registry for the best reachable destination for a role or guest

Routing should depend on:

- trust membership
- live reachability
- health
- policy
- role/capability availability

It should not depend on which hotel happened to do the invitation ceremony months ago.

## Philote And Role Transport

The mesh should support two distinct portability forms:

1. **Work transport**
   - send tasks or replies across the mesh

2. **Identity or role transport**
   - instantiate a philote on another hotel
   - move an active role incarnation to another hotel
   - allow a philote to present one role locally and another remotely when policy allows

This proposal does not force all philotes to become nomads by default.
It does require the mesh membership model to stop being the hidden blocker to that future.

## Security Posture

This proposal does **not** weaken the existing PKI/invite ceremony.

Instead it says:

- trust admission remains explicit and cryptographically grounded
- propagation of membership must also be authenticated and auditable
- revocation must be mesh-wide, not pairwise folklore

If one member can admit another into the shared circle, then one revocation should also remove that hotel from the shared circle in a visible and replayable way.

## First Implementation Seams

1. **Membership propagation**
   - after successful invite acceptance, publish a signed mesh membership record to all current members

2. **Mesh-wide membership storage**
   - add first-class mesh membership records to the shared mesh-visible state model

3. **Global routing view**
   - make hotel membership and reachability resolvable from one converged registry

4. **Role transport contract**
   - define how a philote or role incarnation is represented when transported across hotels

5. **Revocation propagation**
   - make removal of trust global and replayable, not local and whispered

## Why This Matters

Without this shift, Philotic keeps paying the same tax:

- pairwise ceremonies for what operators think is one mesh
- duplicated join work when a new hotel should already be "inside"
- routing ambiguity that has nothing to do with transport quality
- role portability blocked by trust topology rather than policy

The irony is sharp: the system keeps proving the transport path, while the membership model still behaves like a collection of politely adjacent embassies.

## Recommended Next Slice

Build the first mesh-wide membership propagation path:

- keep the bilateral invite ceremony
- emit a signed membership propagation record on acceptance
- replicate that record to all existing members
- make the mesh registry consume and surface those records

That is the smallest honest slice that moves “pairwise trust edges” toward “one singular mesh.”
