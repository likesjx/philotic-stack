---
title: "Telegram Poll Lease Proposal"
doc_type: proposal
domain: membrane-transport
status: accepted-current-slice
last_updated: 2026-03-12
tags:
  - telegram
  - polling
  - leases
  - membrane
  - active-seam
related_docs:
  - ARCHITECTURE_STATUS.md
  - TELEGRAM_INTEGRATION_PROPOSAL.md
  - MEMBRANE_COMPONENT_PROPOSAL.md
  - SESSION_LOOP_PROPOSAL.md
  - RUNTIME_AUTHORITY_LEASES_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: telegram-poll-lease
implements:
  - session-loop
implemented_by:
  - poll-lease-renewal-release-slice
active_seams:
  - telegram-poll-lease
  - delegated-telegram-polling
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
---

# Telegram Poll Lease Proposal

## Goal

Define how Philotic prevents split-brain Telegram polling when multiple membranes could be configured with the same bot token.

This proposal exists to answer four questions cleanly:

- who owns poller authority for a Telegram bot token
- how a membrane acquires and renews that authority
- what happens during shutdown, failover, or stale ownership
- how polling ownership stays separate from session ownership and reply routing

This proposal is mesh-aware, but the preferred authority model is agent-home anchored. The question is not "which role currently has the membrane route," but "which hotel is the stable membrane authority for this agent/bot token, and which poller may advance the Telegram cursor on its behalf right now."

## Core Recommendation

Treat Telegram long-poll ownership as agent-home-hotel-owned lease state.

More precisely:

- the coordinated agent's home hotel owns the canonical poll lease record for each Telegram bot token
- `membrane` performs Telegram `getUpdates` only while it holds a valid lease
- only one active long-poller may exist per bot token at a time
- lease grants must carry a fencing epoch so an old poller cannot keep acting after a new owner is chosen
- standby membranes must not "race politely"; they should wait for lease acquisition instead of polling speculatively
- membranes that cannot acquire or renew through hotel authority must fail closed and stop polling
- one membrane process may hold multiple token-specific leases, but each token keeps its own isolated polling worker, cursor, and fencing state
- changing the active routed incarnation for a session must not implicitly move poll ownership

This keeps Philotic aligned with the existing ownership rules:

- the agent-home `ansible` owns stable membrane transport authority for that agent/token
- `membrane` owns transport execution
- Telegram's update cursor is treated as single-writer state

## Disposition

Accepted for current slice.

Current active work should be tracked in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Current Slice

Current repo truth:

- `ansible` now owns a local hotel runtime poll-lease registry keyed by token fingerprint
- `ansible` now serves that lease through the shared runtime-authority lease abstraction (`LeaseEnvelope` + central provider/observer registry) rather than a Telegram-only in-memory shape
- `membrane` fingerprints the Telegram bot token, requests a lease for a specific `agent_id`, and fails closed if the lease is denied
- agent identity now carries `authority_hotel`, and lease acquisition is denied when the current hotel is not that agent's home authority
- explicit delegated remote polling is now possible as a transitional contract when the agent identity bundle lists the current hotel in `telegram_poll_delegate_hotels`
- membranes now renew their lease on an interval, stale leases expire locally, and takeover after expiry is covered by targeted tests
- membranes now explicitly release their lease on intentional shutdown paths instead of relying only on disconnect cleanup
- lease ownership is released automatically when the owning membrane disconnects
- dead/zombie membrane owners are now dropped from lease authority, and the guest supervisor respawns stale active membrane rows instead of trusting dead PIDs forever
- startup Telegram smoke remains green with this lease gate in place
- the dual-membrane startup smoke is now green under the shared lease abstraction: only one membrane polls a shared token at a time, and standby takeover succeeds after the active owner is retired
- the startup takeover harness now separates desired-state retirement from dead-PID cleanup, which prevents the smoke from accidentally reactivating the retired poller while proving standby takeover

Still incomplete:

- delegated remote polling now exists as an explicit agent-identity allowlist, but canonical mesh-visible poll authority is not implemented yet
- one process may still only exercise one Telegram token in practice even though the proposal allows token-isolated multi-worker evolution later

This is a real authority-plus-renewal-and-release slice with startup smoke coverage and transitional delegation, but it is intentionally not the full delegation/failover architecture yet.

This proposal should now be read as the first concrete specialization of the broader runtime-authority lease pattern in [RUNTIME_AUTHORITY_LEASES_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RUNTIME_AUTHORITY_LEASES_PROPOSAL.md).

## Why This Needs Its Own Authority

Telegram polling is not just another background loop.

With `getUpdates`, the real authority is the update offset. Two pollers sharing one bot token are effectively competing to advance the same cursor. That creates:

- nondeterministic update consumption
- missing or duplicated user-visible behavior
- impossible-to-explain races during restarts
- weak failover semantics

If membranes coordinate this peer-to-peer, the system pushes shared transport authority out to the edge and invites split-brain during startup races or partial failure.

The coordinated agent's home hotel is the better owner because it already owns or should own:

- guest materialization and liveness
- canonical runtime coordination state
- the stable membrane authority for that agent identity
- lease-like authority patterns elsewhere in the architecture

This does not mean the currently active role incarnation should own the poller. Role switching is session routing state, not transport authority. The Telegram poller should remain anchored to the stable agent/home-hotel membrane boundary while the active routed incarnation can change underneath it.

Remote or delegated polling may still be needed, but it should be a ceremony granted by the home hotel, not a generic race between equally eligible hotels.

## Proposed Lease Model

Each Telegram bot token gets one agent-home-hotel-owned lease record:

- `bot_token_ref`
- `agent_id`
- `authority_hotel`
- `owner_guest_id`
- `owner_hotel`
- `lease_epoch`
- `lease_expires_at`
- `last_heartbeat_at`
- `status` (`active`, `releasing`, `expired`)

Recommended behavior:

1. A membrane asks the authority hotel to acquire the poll lease for a specific bot token reference.
2. The authority hotel grants the lease only if no valid active lease exists, or the existing lease has expired.
3. The grant returns a `lease_epoch`.
4. The membrane long-polls only while renewals succeed for that same epoch.
5. If renewal fails, the membrane stops polling immediately.
6. On graceful shutdown, the membrane releases the lease.
7. On crash or partition, the authority hotel reassigns the lease only after expiry or explicit delegation rules.

Preferred steady state:

- the poller runs on the same hotel as the coordinated agent's default conversational/orchestrator incarnation
- the membrane asks that same hotel where inbound turns should route
- role handoff updates the active route target, not the poll lease owner

Allowed but secondary:

- a different hotel runs the poller
- only when the home hotel explicitly delegates that authority and can still supervise its health

One membrane process may repeat this flow for multiple bot token references. That is acceptable as long as each token gets:

- its own lease record
- its own polling worker
- its own update offset/cursor
- its own renewal and fencing decisions

The process may be multi-token. The authority may not be shared across tokens.

## Fencing Rule

Epochs are required.

Without fencing, two membranes can both believe they are primary during a restart race or delayed heartbeat window. That turns "only one poller" into a motivational slogan instead of a system guarantee.

The lease epoch should increment whenever ownership changes. Any membrane that cannot renew its current epoch must stop polling and treat itself as standby.

## Fail-Closed Rule

Membranes must fail closed.

If a membrane:

- starts without hotel connectivity
- cannot acquire a poll lease
- loses renewal for its current epoch
- detects that another epoch has superseded its lease

then it must stop calling `getUpdates` for that token immediately.

It may remain alive as a standby process, but it must not continue polling "just in case." A stray poller without hotel authority is not a degraded success mode; it is an unauthorized cursor writer.

## Boundary With Session Ownership And Role Routing

Telegram poll authority is not the same thing as conversation ownership, and it is not the same thing as active role routing.

Keep these separate:

- poll lease: who may read inbound Telegram updates for a bot token
- membrane transport authority: which agent/home hotel owns that poll lease domain
- session binding: which Philotic session a chat/thread maps to
- active route target: which incarnation currently receives new inbound turns for that session
- reply routing: which membrane target owns outbound delivery for that session

This matters because one membrane may own polling for a token while many sessions are active behind it, and because `/role` handoffs should not accidentally become membrane elections in disguise.

## First Implementation Slice

The first coherent slice should:

1. Add a hotel-owned `telegram_poll_lease` record and query/update path.
2. Add acquire, renew, and release operations for Telegram poll leases.
3. Teach `membrane` to block polling until lease acquisition succeeds and to fail closed when hotel authority is unavailable.
4. Teach `membrane` to stop polling immediately on lost renewal or stale epoch.
5. Add one representation of agent-home membrane authority so poll ownership is anchored to the coordinated agent, not just the current local runtime.
6. Add one smoke that proves only one of two membranes with the same token actively polls under home-hotel authority.
7. Add one failover or delegation smoke that proves a non-home poller only works when explicitly granted by the home hotel.
8. Add one smoke that proves a stray poller without hotel lease authority shuts down or remains non-polling standby.
9. Decide whether the first implementation supports one token per membrane process or multiple token-specific workers in one process, while keeping lease/cursor ownership per token.

## Out Of Scope For This Proposal

- webhook ingress design
- Telegram command catalog behavior
- session reply routing
- per-chat or per-thread role handoff
- multi-transport membrane routing beyond Telegram polling

Those are adjacent, but they should not be silently bundled into poll-lease work.

## Links

- [docs/architecture/TELEGRAM_INTEGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_INTEGRATION_PROPOSAL.md)
- [docs/architecture/MEMBRANE_COMPONENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMBRANE_COMPONENT_PROPOSAL.md)
- [docs/architecture/SESSION_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SESSION_LOOP_PROPOSAL.md)
- [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
