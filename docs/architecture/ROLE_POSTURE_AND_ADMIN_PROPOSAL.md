---
title: Role Posture And Admin Proposal
doc_type: proposal
domain: operator-control-plane
status: proposed
last_updated: 2026-03-31
tags:
- roles
- admin
- posture
- elevation
- active-seam
related_docs:
- ARCHITECTURE_STATUS.md
- AGENT_INCARNATION_PROPOSAL.md
- ROLE_CONTEXT_SHIFT_AND_DELEGATED_SUBAGENTS_WHITEPAPER.md
- CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md
- LOCAL_ADMIN_FALLBACK_MODEL_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: role-posture-and-admin
implements: []
implemented_by: []
active_seams:
- admin-posture-model
- session-admin-elevation
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Role Posture And Admin Proposal

## Goal

Clarify the role model for user-facing conversation, administration, and higher-trust system control so capability creep does not quietly turn the conversational role into an omnipotent gremlin.

## Disposition

`proposed`

Track related work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md) and [AGENT_INCARNATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_INCARNATION_PROPOSAL.md).

## Core Recommendation

Philotic should distinguish at least three durable postures:

1. **conversational role**
2. **admin role**
3. **specialist/worker roles**

The user-facing conversational role should stay intentionally narrow.

## Recommended Role Discipline

### Conversational role

- fixed as the default membrane-facing role
- do not continuously accrete more direct tools
- may gain bounded skills, but only where those skills do not implicitly grant broad system authority
- should escalate or hand off rather than absorb every management capability itself

### Admin role

- explicitly elevated
- owns system management, policy mutation, repair, and high-trust control actions
- should be visible in both architecture and operator UX, not buried as an accidental toolset

Important boundary:

- the admin role is not the root source of authority by default
- operator/session elevation should normally be what activates admin posture
- a trusted automation role may be allowed later, but only as an explicit policy choice

This keeps “admin-capable” distinct from “permanently admin-authoritative.”

### Specialist/worker roles

- capability-specific
- specialist roles are additive postures of the same self and should usually activate via context shift
- should not redefine the meaning of conversational ownership

Important distinction:

- specialist **roles** are same-self postures with shared durable memory
- **workers/subagents** are bounded delegated execution units

Those are related, but they are not the same category wearing different hats because the naming budget ran out.

## Why This Matters

Without this discipline:

- the user-facing role becomes harder to reason about
- safety and approval boundaries blur
- tooling and skills become indistinguishable from authority

That is efficient right up until the conversational incarnation starts acting like root.

## First Slice Recommendation

Make the role posture explicit in:

- role/incarnation records
- tool and skill grants
- membrane routing
- admin UX
- session elevation rules

and treat admin authority as first-class rather than as “whatever role happens to have the scary tools.”

Also keep the split honest:

- role activation narrows focus for one self
- subagent spawning distributes labor under bounded delegation
