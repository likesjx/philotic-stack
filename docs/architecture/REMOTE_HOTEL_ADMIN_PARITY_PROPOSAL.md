---
title: Remote Hotel Admin Parity Proposal
doc_type: proposal
domain: operator-control-plane
status: accepted-current-slice
last_updated: 2026-05-05
tags:
- desktop
- remote-admin
- mesh
- operator-surface
- philotic-web
related_docs:
- DESKTOP_MEMBRANE_PROPOSAL.md
- OPERATOR_MEMBRANE_PLUGIN_BOUNDARY_PROPOSAL.md
- CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md
- DESKTOP_COMPONENT_AUTHORING_PARITY_PROPOSAL.md
- ROUTED_OPERATOR_CHAT_PROPOSAL.md
- ARCHITECTURE_STATUS.md
task_refs:
- docs/task.md
proposal_id: remote-hotel-admin-parity
implements:
- desktop-membrane
implemented_by: []
active_seams:
- remote-hotel-admin-parity
- target-scoped-admin-mutations
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# Remote Hotel Admin Parity Proposal

## Goal

Make the Philotic desktop/operator surface able to manage a remote `aiua` through the mesh with the same conceptual clarity it already has for local administration, without bypassing target-hotel authority.

This is not a claim that the desktop should speak raw remote-hotel protocol directly. It is a claim that remote hotel administration should become a first-class control-plane story instead of a scattered set of mesh-aware read routes plus a collection of operator wishes.

## Core Recommendation

Treat remote hotel administration as a first-class **hotel-mediated control-plane surface**.

Recommended shape:

1. the desktop membrane binds to one local operator session and one local hotel authority lease
2. that local hotel exposes reusable operator surfaces for both local and remote targets
3. remote reads and mutations route through explicit target-aware control-plane contracts
4. the target hotel remains canonical for its own state, policy, validation, mutation, and audit
5. the desktop never opens a browser-direct remote `aiua` control channel just because it would feel efficient for five minutes

The desktop should therefore feel like one mesh-native admin app, while the implementation remains hotel-mediated and target-scoped rather than ambiently godlike.

## Disposition

`accepted for current slice`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Why This Matters

Right now the repo truth is in a transitional but useful middle ground:

- the desktop membrane can inspect mesh targets
- it can query target status, guests, and agents
- it can route operator chat to remote agents
- component authoring parity exists for the local hotel surface

That is enough to prove the direction and not enough to count as first-class remote hotel administration.

The irony is familiar: we have a mesh-aware desktop that can already talk across hotels, but “manage a remote `aiua`” is still distributed across proposal prose, route fragments, and whatever the operator can remember under mild sleep deprivation.

## First-Class Remote Admin Means

The desktop should be able to do these things against a remote target hotel through the same mesh-aware operator posture:

### Remote hotel inspection

- target status
- target guest/component inventory
- target agent inventory
- target config presence and safe summaries
- target secret/vault-ref inventory without leaking plaintext
- target placement/health/routing signals

### Remote hotel mutations

- component create/update/delete
- component enable/disable/restart
- bounded config mutation for operator-approved keys
- secret rotation through target-hotel-owned secret flows
- role and philote governance actions
- routed placement choices such as `hotel.best_place_to_run`

### Remote hotel transport and placement

- hand off an agent or role to a target hotel
- request materialization on the best hotel
- observe readiness, failure, and attribution

The point is not to produce a giant button wall. The point is to make remote hotel administration an explicit contract family instead of a pile of one-off membrane exceptions.

## Non-Negotiable Rules

### 1. Target hotel authority is preserved

Every remote read or mutation must still be owned and validated by the target hotel.

The local desktop membrane may:

- route
- request
- present
- aggregate

It may not silently become the mutation authority for a remote hotel because the UI happens to look centralized.

### 2. One operator app, not many sockets

The desktop should feel like one mesh admin app.

That does **not** mean:

- browser-direct remote `aiua` protocol
- one credential set per remote hotel in the browser
- independent authority models per open tab

The desktop should attach locally and let the hotel/router mediate remote work.

### 3. Reads and writes get different contracts

Remote inspection and remote mutation should not be the same surface wearing a moustache.

Remote reads want:

- attribution
- freshness
- redaction
- canonical/fallback state labeling

Remote writes want:

- target-scoped grants
- posture/elevation checks
- validation
- auditable mutation envelopes
- explicit completion/failure reporting

### 4. Secrets remain secret-shaped

The desktop may inspect presence, refs, and rotation workflows.

It should not normalize plaintext secret fetches from remote hotels into the default admin experience just because moving membranes to `jane-vps` is inconvenient. Inconvenience is not an architecture.

## Current Repo Truth

Current implemented slices already prove parts of the path:

- [DESKTOP_MEMBRANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DESKTOP_MEMBRANE_PROPOSAL.md) establishes mesh-aware desktop reach and local lease authority
- [OPERATOR_MEMBRANE_PLUGIN_BOUNDARY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/OPERATOR_MEMBRANE_PLUGIN_BOUNDARY_PROPOSAL.md) establishes reusable target-oriented operator surfaces instead of desktop-specific daemon drift
- [ROUTED_OPERATOR_CHAT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROUTED_OPERATOR_CHAT_PROPOSAL.md) proves a routed remote operator interaction path
- [DESKTOP_COMPONENT_AUTHORING_PARITY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DESKTOP_COMPONENT_AUTHORING_PARITY_PROPOSAL.md) proves local manifest-authoring parity

What is still missing is the explicit parity story for remote hotel administration itself.

## Current Slice

Define remote hotel admin parity as an explicit operator-control-plane seam and make it the next desktop membrane/admin target.

First honest slice:

1. define the remote admin contract family
2. land remote component inventory/detail parity through the existing `operator.targets.*` control-plane path
3. land the matching remote component mutations (`create`, `update`, `delete`, `enable`, `disable`, `restart`) through that same control-plane family
4. enumerate which existing local desktop mutations should gain remote parity next
5. keep target-scoped grants explicit
6. keep secret handling ref-shaped and target-owned
7. hook placement and handoff decisions into the same surface

This slice is intentionally architectural and sequencing-oriented, not a claim that the whole remote admin plane is implemented today.

## Recommended First Remote Parity Set

Land these in order:

1. remote component inventory + detail parity
  Current truth: landed through `/api/mesh/targets/:target_node_id/components` plus `/api/mesh/targets/:target_node_id/components/:guest_id`, backed by the same hotel-mediated operator query seam as target status/guest/agent reads.
2. remote component mutations (`create`, `update`, `delete`, `enable`, `disable`, `restart`)
  Current truth: landed through matching target-scoped mesh routes and operator-surface mutation requests, keeping the target hotel authoritative for all component writes instead of teaching the desktop a second remote mutation dialect.
3. remote config read/mutate parity for bounded operator-approved keys
  Current truth: landed through `/api/mesh/targets/:target_node_id/config` plus `PUT /api/mesh/targets/:target_node_id/config/:key`, backed by the same target-hotel operator surface and intentionally bounded to approved non-secret keys (`execution_host`, `tool_runner_registry`, plus read-only `vault_registry` visibility).
4. remote secrets/vault-ref inventory and rotation workflows
  Current truth: landed through `/api/mesh/targets/:target_node_id/secrets`, `POST /api/mesh/targets/:target_node_id/secrets/rotate`, and `POST /api/mesh/targets/:target_node_id/vault`, with inventory returning only metadata/refs and target-hotel authority owning rotation or vault-entry creation. Plaintext fetches remain intentionally absent.
5. remote placement and role transport actions

That ordering keeps us moving from established read models toward higher-agency mutations without pretending every remote action deserves to be born in one giant ceremony.

## Relationship To Mesh Placement

Remote hotel admin parity is not just an operator-surface concern.

It is one of the missing pieces that lets:

- agents live on one hotel
- roles materialize on another
- membranes concentrate on `jane-vps`
- local resource-bound runners stay on laptops

without making the operator use a separate folklore playbook for every machine.

So this proposal is directly adjacent to:

- remote materialization
- best-place-to-run
- philote/role transport
- ghost-mirror target intelligence

## Open Questions

1. Which remote mutations should require explicit target-scoped grants beyond elevated operator posture?
2. Which remote actions should be async job-style with progress streaming instead of synchronous mutation replies?
3. Should remote component authoring reuse the exact `/api/components` shape with target scoping, or should target-scoped admin routes get their own namespace first?
4. How should the desktop present “target canonical” versus “routed fallback” truth so operators do not mistake reachability gossip for real remote state?
