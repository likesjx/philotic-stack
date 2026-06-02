---
title: Operator Identity And Dangerous Action Ceremonies Proposal
doc_type: proposal
domain: operator-control-plane
status: proposed
last_updated: 2026-05-05
tags:
- desktop
- operator-identity
- admin-posture
- action-grants
- dangerous-actions
- remote-admin
related_docs:
- DESKTOP_MEMBRANE_PROPOSAL.md
- REMOTE_HOTEL_ADMIN_PARITY_PROPOSAL.md
- CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md
- ROLE_POSTURE_AND_ADMIN_PROPOSAL.md
- ARCHITECTURE_STATUS.md
task_refs:
- docs/task.md
proposal_id: operator-identity-and-dangerous-action-ceremonies
implements: []
implemented_by: []
active_seams:
- desktop-operator-identity
- admin-session-posture
- dangerous-action-ceremonies
- target-scoped-action-grants
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# Operator Identity And Dangerous Action Ceremonies Proposal

## Goal

Define the first honest long-term model for:

- who is operating the Philote desktop
- what posture that operator session carries
- which dangerous actions need extra ceremony
- when typed confirmation is enough
- when a target-scoped action grant or stronger step-up flow is required

The point is to stop treating “the desktop is basically admin” as either a permanent architecture or a guilty secret.

## Core Recommendation

Treat desktop administration as a **bounded operator session** with three distinct layers:

1. **operator identity**
2. **session posture**
3. **dangerous-action ceremony**

Recommended shape:

1. a desktop session resolves to a concrete operator identity whenever possible
2. that session begins in a normal posture and may elevate into admin posture for a bounded lifetime
3. dangerous actions always require explicit ceremony, even for an admin session
4. the ceremony gets stronger as the blast radius increases:
   - typed confirmation for sharp but routine actions
   - target-scoped grants for high-trust remote actions
   - later, optional step-up auth or break-glass ceremony for the truly spicy ones
5. target hotels remain authoritative for final validation and execution of dangerous remote actions

The desktop should feel like one coherent operator app, not like a permanent god token with attractive typography.

## Disposition

`proposed`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Why This Needs Its Own Proposal

We already landed the first practical safety slice:

- typed confirmation for local and remote component restart/delete
- typed confirmation for secret rotation
- typed confirmation for vault entry creation
- typed confirmation for remote role-home moves

That is good and correct, but it is not the whole long-term model.

What remains unresolved is larger:

- do we have named human operators or just one ambient desktop principal
- how are operator sessions represented
- how does admin posture elevate and expire
- which actions need target-scoped grants instead of confirmation alone
- which actions need stronger ceremony than either of those

If we leave that all inside remote-admin parity docs, we get one of those classic architectures where the real security model is smeared across routes, comments, and operator folklore. The software equivalent of “I’m sure we talked about it once.”

## Current Truth

Today’s repo/runtime truth is:

- the desktop membrane is the operator surface
- a successful desktop session is effectively admin-capable
- dangerous actions now require typed confirmation on the HTTP surface
- remote actions still execute through target-hotel authority

That means current safety is:

- better than ambient trust
- not yet a full operator identity and grant model

So the honest statement is:

`typed confirmation is the first safety slice, not the final security model`

## Proposed Model

### 1. Operator Identity

Philotic should grow a first-class operator identity model for desktop/admin work.

The first useful shape can stay lightweight:

- named operator identity
- local login/session attachment
- optional later mapping to OS identity, passkey, keychain-backed proof, or external auth

The important thing is not a giant user database on day one.
The important thing is that audit and policy stop pretending every desktop session is the same mysterious benevolent force.

### 2. Session Posture

A desktop session should carry posture that is explicit and bounded.

Recommended initial posture vocabulary:

- `normal`
- `admin_elevated`
- later, `break_glass`

Recommended rules:

- sessions begin as `normal`
- admin posture is explicit, time-bounded, and revocable
- inactivity expires elevated posture
- handoff across hotels does not automatically imply the far target accepts the action

### 3. Dangerous-Action Ceremony Ladder

Not all risky actions deserve the same ceremony.

Recommended first ladder:

#### Tier 0: normal admin posture only

- read inventory/status/health
- read bounded remote config summaries
- perform low-risk inspection actions

#### Tier 1: typed confirmation

- component restart/delete
- secret rotation
- vault entry creation
- remote role-home mutation

This is the current implemented slice.

#### Tier 2: target-scoped action grant

- remote secret revoke/add with wider blast radius
- remote guest migration
- remote node shutdown/restart
- mesh membership invite/revoke
- perimeter or routing policy mutation

These actions should require a short-lived target-scoped grant bound to:

- operator identity
- session id
- target hotel
- action class
- optional specific resource
- expiry / one-time use

#### Tier 3: break-glass or step-up ceremony

- actions that can strand the operator, destroy trust topology, or cut off safety paths

Examples:

- revoke the last trusted operator path
- wipe or replace mesh trust roots
- destructive recovery paths

This tier is intentionally not implemented yet; the point is to name it before the need arrives with a flamethrower.

## Relationship To Existing Proposals

This proposal does not replace:

- [REMOTE_HOTEL_ADMIN_PARITY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/REMOTE_HOTEL_ADMIN_PARITY_PROPOSAL.md)
  - that proposal is about the control-plane surface and parity
- [DESKTOP_MEMBRANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DESKTOP_MEMBRANE_PROPOSAL.md)
  - that proposal is about the desktop membrane boundary and lease
- [CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md)
  - that proposal establishes the broader admin-surface and grant philosophy
- [ROLE_POSTURE_AND_ADMIN_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROLE_POSTURE_AND_ADMIN_PROPOSAL.md)
  - that proposal establishes posture discipline at the role/session level

This proposal is the seam where those threads become one concrete large-scale effort for the desktop and mesh operator story.

## Current Slice

This proposal does not ask us to implement the whole ceremony stack now.

It asks us to package the next effort honestly:

1. define a desktop operator identity/session model
2. define posture transitions and expiry rules
3. define the dangerous-action ladder and first target-scoped grant classes
4. classify current admin actions by ceremony tier
5. choose the first end-to-end grant-backed remote action
6. add operator-visible audit attribution across the desktop and target hotel surfaces

## First Recommended Work Breakdown

1. **Identity slice**
   - decide first operator identity source
   - define session record shape
   - expose operator identity in audit and event trails

2. **Posture slice**
   - explicit `normal` vs `admin_elevated`
   - bounded TTL and expiry behavior
   - visible desktop posture state

3. **Grant slice**
   - define first target-scoped grant record
   - define issuance, consumption, expiry, and denial semantics
   - pick one action class and prove the whole loop

4. **Ceremony UX slice**
   - make typed confirmation, grant request, pending approval, and denial/success states legible in the desktop

## Open Questions

1. What should be the first operator identity source:
   - local desktop login only
   - named operator profile in graph/config
   - OS-account-backed identity
2. Which current dangerous actions should remain typed-confirm-only versus moving to target-scoped grants first?
3. Should admin posture be hotel-local, mesh-wide, or mesh-brokered but target-validated?
4. Which actions deserve full break-glass ceremony, and how do we keep that from becoming ambient admin with a slightly more dramatic button?
