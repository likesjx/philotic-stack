---
title: "Runtime Authority Leases Proposal"
doc_type: proposal
domain: runtime-sessions
status: accepted-current-slice
last_updated: 2026-03-12
tags:
  - leases
  - runtime
  - authority
  - supervision
  - active-seam
related_docs:
  - ARCHITECTURE_STATUS.md
  - SESSION_LOOP_PROPOSAL.md
  - TELEGRAM_POLL_LEASE_PROPOSAL.md
  - KEY_VAULT_PROPOSAL.md
  - MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: runtime-authority-leases
implements: []
implemented_by: []
active_seams:
  - runtime-authority-leases
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
---

# Runtime Authority Leases Proposal

## Goal

Define a reusable Philotic pattern for runtime authority over singleton or bounded-owner work such as:

- transport pollers
- active session workers
- materialized runners that should not be multiply active
- delegated component ownership that must be revocable

This proposal is not trying to turn the entire platform into one giant lease factory. It is trying to name the pattern we are already using when one authority must grant one actor the right to act right now.

## Core Recommendation

Philotic should adopt **runtime authority leases** as a general control-plane archetype for resources that need:

- one canonical grantor
- one current owner within a defined scope
- explicit expiry or revocation
- heartbeat-based liveness
- fencing against stale actors

Each lease should answer:

- who may act
- on what scoped resource
- under which authority
- until when
- with what current epoch

This proposal therefore requires a **shared lease abstraction**, not only matching prose.

Minimum required shape:

- one shared `LeaseEnvelope` record model
- one shared provider contract for acquire/renew/release/revoke/inspect
- one shared observer hook vocabulary for owner-change, expiry, revoke, and stale-owner cleanup

Without that, Philotic would have several lease-shaped systems and a proposal insisting they are the same by force of optimism.

This pattern should be used for runtime authority. It should not silently absorb:

- secret storage
- durable business records
- arbitrary routing metadata
- append-only event history

Those are adjacent concerns, not the same thing wearing a fake mustache.

This also does **not** mean every materialized component needs an exclusive acting lease.

Philotic likely needs at least two related lease families:

- **authority leases**
  - who may act inside a scoped runtime domain right now
- **retention leases**
  - how long a materialized instance is allowed to stay alive before it should be reclaimed or downscaled

Some components need both. Some need only retention. Treating those as one universal lease would be elegant in exactly the way that causes category errors later.

## Disposition

Accepted for current slice.

## Current Slice

Current repo truth for this slice:

- `philotic-client` now defines a shared `LeaseEnvelope` and `LeaseStatus`
- `ansible` now has a central runtime lease registry/provider with shared acquire, renew, release, inspect, and observer-hook vocabulary
- Telegram poll lease now uses that shared lease abstraction instead of a private ad hoc registry shape
- the startup dual-membrane poll-lease smoke is green again under the shared abstraction after tightening stale-owner detection and startup handoff behavior
- the proposal now carries the first explicit boundary contract separating lease authority from materialization, supervision, routing, and vault access
- session leases are still conceptually aligned with the archetype, but are not yet migrated onto the shared provider path

This slice defines and proves the shared archetype that already underlies session leases and Telegram poll leases, and makes explicit how leases interact with:

- materialization
- supervision
- revocation and failover
- mesh-visible owner projections
- future lease-scoped secret access without conflating leases with vault authority

Linked task surface: [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)

## Why This Matters

Telegram poll lease exposed a broader platform truth:

- we need a deterministic way to decide which component may act
- we need the authority hotel to stop, replace, or reassign that actor
- we need stale actors to fail closed instead of continuing optimistically

That same shape applies beyond Telegram.

Without a named archetype, each component seam will reinvent:

- ownership fields
- heartbeat semantics
- expiry behavior
- kill/restart authority
- stale-owner cleanup

Eventually Philotic would have many lease-like systems and none of them would quite admit it.

## Lease Archetype

A runtime authority lease should have these core properties:

1. **Canonical grantor**
   - one component or hotel authority grants and revokes the lease
2. **Scoped resource**
   - the lease names exactly what resource or work domain it controls
3. **Single current owner**
   - one owner at a time unless the lease type explicitly allows bounded multiplicity
4. **Fencing epoch**
   - each reassignment increments an epoch so stale owners can detect loss of authority
5. **Expiry and renewal**
   - the owner must renew before expiry or lose the right to act
6. **Fail-closed behavior**
   - inability to renew means stop acting, not keep going hopefully
7. **Inspectable current owner**
   - the authority can expose who owns the lease and whether it is healthy

Suggested common fields:

- `lease_type`
- `lease_scope`
- `authority_hotel`
- `owner_guest_id`
- optional `owner_hotel`
- `lease_epoch`
- `lease_expires_at`
- `last_heartbeat_at`
- `status`

## Lease Families

### Authority leases

Authority leases answer:

- who may act
- on what scope
- under which authority
- with which epoch

Use authority leases when duplicate active actors would be incorrect, dangerous, or confusing.

Current and likely examples:

- Telegram poller per bot token
- active agent/session actor per scoped conversation or role
- singleton coordinator per domain

### Retention leases

Retention leases answer:

- how long a materialized instance may remain resident
- what demand or policy is renewing its residency
- when it should drain, sleep, or retire

Use retention leases when multiple active instances are acceptable, but runaway residency or idle sprawl is not.

Likely examples:

- model workers
- tool runners
- warm standby helpers
- prewarmed remote capacity opened for latency relief

### Why both matter

This distinction lets the platform say:

- an agent may be `authority-bound` and also subject to retention policy
- a model worker may be replicated for routing, but still reclaimed deterministically when demand fades
- a standby poller may be materialized under a retention policy without holding acting authority

Without this split, downscaling pressure tempts us to put exclusive authority leases on everything just to get a reclaim hook, which is a very tidy misuse of the abstraction.

## Required Shared Structure

Yes, there should be one shared lease envelope and a small set of lease-facing roles around it.

Recommended split:

- **lease envelope**
  - the portable record shape for one granted or inspectable lease
- **lease provider**
  - the canonical authority that grants, renews, releases, revokes, and reports ownership
- **lease holder**
  - the component currently acting under the lease
- **lease observer**
  - any component that needs to inspect ownership, freshness, or state transitions without becoming the grantor

That gives us a common control shape without pretending every lease family has the same storage backend or failure policy.

### Lease envelope

Suggested portable shape:

```json
{
  "lease_type": "telegram_poll",
  "lease_scope": "telegram:bot-token-ref:sha256:abcd",
  "authority_hotel": "hotel-alpha",
  "owner_guest_id": "membrane-telegram-01",
  "owner_hotel": "hotel-alpha",
  "lease_epoch": 7,
  "lease_expires_at": 1741810200,
  "last_heartbeat_at": 1741810170,
  "status": "active"
}
```

Suggested role-agnostic fields:

- `lease_type`
- `lease_scope`
- `authority_hotel`
- optional `authority_component`
- `owner_guest_id`
- optional `owner_hotel`
- optional `owner_component_type`
- `lease_epoch`
- `lease_expires_at`
- `last_heartbeat_at`
- `status`
- optional `delegated_from`
- optional `metadata`

Recommended additional field when the family matters explicitly:

- `lease_family`
  - `authority`
  - `retention`

### Lease provider

The provider owns the canonical lease lifecycle.

Recommended responsibilities:

- `acquire`
- `renew`
- `release`
- `revoke`
- `inspect`
- stale-owner cleanup
- epoch increment on ownership change
- optional watcher notification hooks

This is usually `ansible`, but the pattern should care about the role, not the binary name.

At the implementation level, this can start as:

- a shared Rust struct for `LeaseEnvelope`
- a provider trait or service contract
- domain-specific adapters for Telegram poll leases, session leases, and future lease families

It does not need a fully generic storage engine on day one. It does need a real contract.

### Lease holder

The holder is the active actor using the lease.

Recommended holder hooks:

- `on_granted`
- `on_renewed`
- `on_lost`
- `on_revoked`
- `on_expired`

The important behavioral rule is simple:

- if the holder loses lease authority, it must stop acting inside that scope

For retention-oriented holders, the corresponding rule is:

- if the holder loses retention, it must drain, sleep, or retire according to policy instead of assuming indefinite residency

### Lease observer

Observers are how supervision, placement, routing, and admin surfaces can watch lease truth without becoming the authority.

Recommended observer hooks:

- `on_owner_changed`
- `on_expired`
- `on_revoked`
- `on_stale_owner_dropped`

Likely observers include:

- guest supervision
- materialization policy
- admin/control-plane views
- mesh-visible state projection

### Why provider and observer should stay separate

If the same generic object tries to be grantor, watcher, materializer, router, and secret issuer all at once, it stops being an abstraction and starts becoming a small monarchy with serialization.

The cleaner model is:

- provider grants truth
- holder acts under truth
- observer reacts to truth

That separation gives us hooks without creating another magic runtime blob.

## Relationship To Materialization And Supervision

Leases are not materialization, but they should inform it.

Recommended boundary:

- materialization decides which guest/process should exist
- supervision decides whether it is healthy and when to restart or kill it
- lease authority decides whether that guest/process is currently allowed to act

Retention policy fits beside those boundaries:

- retention decides how long an existing guest/process is worth keeping around when it is idle or on standby

That means the hotel can:

- materialize a standby component without granting it authority yet
- revoke a lease before killing a component
- kill a component whose lease it lost
- rematerialize a component and re-grant authority under a new epoch
- reclaim replicated capacity because retention expired without pretending that the instance ever had exclusive acting authority

This is exactly why the pattern is useful: it lets control stay boring and deterministic even while processes are being dramatic.

## Boundary Contract With Materialization, Supervision, Routing, And Vault

This is the first explicit boundary contract for the lease archetype.

The platform should treat these as adjacent but separate owners:

| Concern | Canonical job | May observe | Must not silently become |
| --- | --- | --- | --- |
| Lease authority | decide who may act right now inside a scoped runtime domain | holder liveness, expiry, revoke signals, stale-owner evidence | the materializer, router, or vault |
| Materialization | decide which guest/process should exist and where it should run | desired placement, lease state, route demand, readiness prerequisites | the lease grantor or the session/router owner |
| Supervision | decide whether a materialized process is healthy, stale, or needs restart/reclaim | process health, registration, heartbeats, lease-holder behavior | the lease owner selector or route planner |
| Routing | decide where work should go | lease visibility, readiness, placement, session ownership | the lease authority or process supervisor |
| Vault/secret access | decide what secret material exists and under which policy it may be accessed | lease status, requester identity, grant scope | the lease itself or the materializer |

Short version:

- lease authority grants permission to act
- materialization creates or removes candidates that could act
- supervision keeps those candidates honest and alive
- routing selects where work should go
- vault access decides what credentials can be issued or resolved

That sounds obvious, which is usually a sign that architecture is finally becoming useful.

### Lifecycle states

For a lease-scoped runtime component, the recommended lifecycle is:

1. `desired`
   - policy or routing says this component should exist
2. `materializing`
   - hotel is starting or placing the component
3. `ready-standby`
   - process exists and is healthy, but does not yet hold authority
4. `leased-active`
   - process holds the lease and may act inside the scoped domain
5. `degraded`
   - process still exists but is unhealthy, cannot renew, or should be drained
6. `releasing`
   - lease is being handed off or intentionally dropped
7. `retired`
   - process is no longer desired or should no longer run

Not every component needs every state on day one, but the distinction between `ready-standby` and `leased-active` matters. A materialized process is not automatically authorized just because it exists and is feeling optimistic.

### Allowed interactions

The intended interaction rules are:

- materialization may create a standby process before any lease is granted
- lease authority may deny or revoke a lease without immediately killing the process
- supervision may kill or restart a process without changing route ownership by itself
- routing may prefer a leased-active target, or request materialization of a standby target, without self-granting authority
- vault access may require a valid lease for a scoped credential request, but the lease does not create or store the secret

### Forbidden shortcuts

The system should avoid these shortcuts:

- a router granting lease authority because it picked a destination
- a supervisor reassigning ownership just because a process died
- a materializer assuming spawned means authorized
- a lease provider issuing secrets directly as if it were the vault
- a vault grant silently moving routing or materialization state

If one of those is needed, it should happen as an explicit cross-surface call, not as a side effect hidden in one subsystem.

### Minimal handoff sequence

When a scoped active owner must be replaced, the intended order is:

1. routing or policy marks the current owner for drain or replacement
2. lease authority revokes or lets the old lease enter `releasing`
3. holder stops acting
4. supervision confirms the old holder is quiesced or dead
5. materialization ensures the replacement candidate exists and is ready
6. lease authority grants the new epoch to the replacement
7. routing begins sending new work to the replacement

This ordering is intentionally boring. The alternative is to let restart timing become a decision engine, which is a fantastic way to discover folklore.

## Relationship To Secrets And Keys

Keys are a separate archetype.

Leases may eventually gate access to secret material:

- a component with a valid lease may request lease-scoped credentials
- loss of lease may invalidate that access

But the lease is still not the vault.

Keep these separate:

- lease: who may act now
- vault: what secret material exists and who may access it
- materialization: what process exists
- routing: where work should go

If we collapse those into one object, every future bug will arrive pre-bundled with authority confusion.

## First Candidate Lease Families

These are the clearest current fits:

- session active-work leases
- Telegram poll leases
- future singleton transport consumers
- runner/materialization ownership where only one active executor should exist per scope
- retention leases for replicated model/tool capacity that should downscale cleanly

Possible later fits:

- delegated admin workers
- bounded component leaders

Non-fits by default:

- secret metadata
- context-graph durable records
- general capability advertisements

## Authority Profiles

Leases are not universal in one single shape. Components should instead declare an authority profile.

Useful first-cut categories:

- `singleton-scoped`
  - one active actor per scope
  - requires an authority lease
- `replicated`
  - multiple active actors allowed
  - usually does not require an authority lease
  - may still require a retention lease
- `leader-elected`
  - many materialized candidates may exist
  - one leader acts at a time
  - authority lease or leader-election equivalent still required
- `manual-authority`
  - ownership moves only by policy/admin action

Useful examples:

- agents: usually `singleton-scoped`
- Telegram pollers: `singleton-scoped`
- model workers: usually `replicated`
- tool runners: usually `replicated`
- standby poller/process: materialized under retention policy, but not active without authority

## Mesh Visibility Recommendation

Runtime authority leases should publish mesh-visible owner metadata, not necessarily the full local lease ledger.

At minimum, remote observers should be able to inspect:

- `lease_type`
- `lease_scope`
- `authority_hotel`
- current owner identity
- `lease_epoch`
- `status`
- freshness/health timestamps

This proposal depends on the broader classification work in [MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md). The lease pattern defines authority mechanics; the mesh-visibility proposal defines how much of that truth should be projected and how.

## First Implemented Specialization

Telegram poll lease is the first implemented specialization of this archetype.

Current repo truth already proves:

- explicit acquire, renew, release
- fenced epochs
- fail-closed behavior
- graceful shutdown release
- dead-owner cleanup
- authority tied to the agent's home hotel
- delegated remote authority as an explicit exception

That makes Telegram the proving ground, not the final definition.

## Current Comparison: Session Lease vs Telegram Poll Lease

The current proposals already show the same lease archetype in two different scopes:

| Archetype field / behavior | Session lease | Telegram poll lease | Comparison note |
| --- | --- | --- | --- |
| Canonical grantor | `ansible` session authority | `ansible` poll authority | same control-plane owner shape |
| Scoped resource | one `session_id` / active turn domain | one Telegram bot token / poll cursor domain | both are narrow scoped runtime authority |
| Current owner | active `agent-core` turn worker | active `membrane` poller | different component families, same single-owner idea |
| Renewal | heartbeat-style renewal is proposed | renewal is implemented | same contract, different maturity |
| Expiry | expired lease allows requeue or recovery | expired lease allows takeover by another poller | same safety mechanism, different operational consequence |
| Fencing epoch | implied by single active owner semantics, not yet surfaced as strongly | explicit epoch/fencing is implemented | session lease should likely adopt more explicit fencing language |
| Fail-closed behavior | worker should stop owning active turn if lease is lost | poller stops calling `getUpdates` immediately on loss | same principle, Telegram currently proves it more concretely |
| Graceful release | release is part of lifecycle handoff/completion semantics | explicit release on shutdown is implemented | both need clean release paths, Telegram is ahead |
| Dead-owner cleanup | hotel can requeue after crash/restart | dead/zombie poller owners are dropped and takeover allowed | same recovery pattern with transport-specific checks |
| Mesh-visible owner need | useful for recovery/admin insight, but less urgent today | important for cross-hotel split-brain prevention | both fit the archetype, Telegram currently has more pressure |

Summary:

- session leases protect **active cognitive work**
- Telegram poll leases protect **externally serialized ingress authority**

Same archetype, different blast radius.

Telegram needs stricter explicit fencing because the external cursor is unforgiving. Session leases need the same ownership discipline, but their recovery path is more naturally tied to requeue and checkpoint restore.

Mapped to the shared structure:

- session lease
  - provider: hotel/session authority
  - holder: active `agent-core` worker
  - observers: session recovery, queue/requeue logic, admin/session inspection
- Telegram poll lease
  - provider: hotel/membrane authority
  - holder: active `membrane` poll worker
  - observers: guest supervision, materialization, mesh-visible poll-owner projection

## Comparison Outcome

The comparison suggests three concrete conclusions:

1. session leases and Telegram poll leases should share vocabulary and core record shape
2. session leases should probably become more explicit about fencing/epoch semantics instead of relying on “one active owner” as an informal promise
3. the lease proposal should standardize a real shared envelope/provider/observer abstraction, not just a naming convention
4. the next non-Telegram adopter should be a seam where stale authority can cause real duplicate action, not just inconvenience

## First Slice Recommendation

Before broad implementation:

1. adopt this lease vocabulary in the docs
2. position Telegram poll lease as the first specialization
3. compare session leases against the same archetype and identify shared fields/behavior
4. define one boundary note for how lease-scoped secret access should relate to vault work later
5. implement the first shared lease abstraction:
   - shared `LeaseEnvelope`
   - provider contract
   - observer hook vocabulary
6. adapt Telegram poll lease to that shared abstraction without regressing current smoke coverage
7. decide whether session leases should be the next adopter or whether one intermediate runtime seam is a better proving ground

The first implementation beyond Telegram should be whichever next seam most clearly needs revocable single-owner runtime authority, not whichever component happens to say “lease” in a comment.
