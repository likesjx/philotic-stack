---
title: Control Plane Admin Surface Proposal
doc_type: proposal
domain: operator-control-plane
status: proposed
last_updated: 2026-03-31
tags:
- admin
- control-plane
- cli
- tui
- active-seam
related_docs:
- ARCHITECTURE_STATUS.md
- ROLE_POSTURE_AND_ADMIN_PROPOSAL.md
- LOCAL_ADMIN_FALLBACK_MODEL_PROPOSAL.md
- PERIMETER_EGRESS_CONTROL_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: control-plane-admin-surface
implements: []
implemented_by: []
active_seams:
- cli-tui-admin-surface
- action-grant-contract
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Control Plane Admin Surface Proposal

## Goal

Define the first deterministic management surface for the context graph and hotel/agent runtime so operators stop relying on file edits, restart rituals, and interpretive debugging as the main admin interface.

## Disposition

`proposed`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Core Recommendation

Philotic should have a **deterministic context graph manager** and **admin surface** as part of the main CLI/TUI story.

The first recommended shape is:

1. CLI-backed control plane
2. TUI as the first serious admin app
3. web/app surfaces later if justified

## What This Surface Owns

- inspect graph-backed agent and hotel state
- inspect live materialization/routing state
- mutate allowed admin-controlled records through validated commands
- show drift between startup overlays and graph truth
- expose audit-friendly diffs and repair actions

It should also own high-trust operator workflows that must not be reduced to ordinary chat commands or raw key handling:

- secret add/rotate/revoke initiation
- vault status and audit inspection
- transport/perimeter trust changes
- break-glass or recovery flows with stronger ceremony

## TUI Recommendation

Yes, the TUI should be the first admin app.

Recommended reasons:

- lower implementation cost than web first
- works well for operator workflows
- fits the existing CLI/runtime culture
- can define the control model before future GUI layers add gloss

## Relationship To The Main CLI

The TUI should live under the main Philotic CLI rather than as a disconnected side tool.

That keeps:

- auth and operator posture consistent
- shared config discovery in one place
- admin workflows visible and scriptable

## Future Surfaces

Possible later layers:

- web GUI for richer inspection
- full desktop/mobile app if operator use justifies it

But the architecture should be proven in the CLI/TUI first.

## Relationship To Membrane

`membrane` may expose admin/operator entry points, but it should not become the owner of secret or policy truth.

Recommended boundary:

- `membrane` may start an authenticated operator control session
- `membrane` may launch a Mini App or secure action link
- `membrane` may collect approval intents and control-plane requests
- `aiua` / hotel control plane validates, authorizes, persists, audits, and executes the requested admin action

This keeps the outside-world interface useful without letting the transport boundary quietly become the admin database with better emojis.

Important corollary:

- a hotel without `membrane` must still remain fully administrable
- CLI/TUI control-plane entrypoints are first-class admin surfaces, not degraded backups
- `membrane` is optional ingress for admin, not the source of admin authority

## Elevation Model

Admin authority should not exist as a permanent ambient property of an agent or chat.

The recommended model is:

1. a **principal** requests elevation
2. a **session** may carry elevated admin posture for a bounded time
3. the **hotel** decides whether elevation is allowed
4. dangerous actions use short-lived **action grants**

### Principal

The principal is who is asking for elevation.

Default principal types:

- human operator
- later, explicitly trusted automation or service principals

The default should remain human-first. Trusted automation can be added later, but only by explicit policy rather than by treating an agent role as inherently authoritative.

### Session

The session is where elevation lives once granted.

That means:

- elevation belongs to the session, not permanently to the focused agent
- elevation can expire
- elevation can be revoked
- elevation can be constrained to one channel, hotel, and action class

This keeps authority tied to a specific authenticated interaction rather than to a personality wearing an admin costume.

### Posture

Recommended session postures:

- `normal`
- `admin_elevated`
- later, possibly `break_glass`

Sessions should begin as `normal`.

Even an eligible operator session should require explicit elevation before high-trust actions become available.

### Action Grant

Once a session is elevated, the hotel should mint short-lived signed grants for especially sensitive actions.

These grants should be bound to:

- principal identity
- session id
- hotel id
- allowed action class
- short expiry
- optional nonce / one-time use semantics

This lets the system prove “this exact session may perform this exact dangerous action right now” without handing the session a permanent treasury key.

## First Action-Grant Contract

The first implementation should make action grants explicit enough that they can become canonical hotel records later, instead of hiding them in ad hoc callback payloads.

Suggested first record shape:

```json
{
  "grant_id": "grant_01...",
  "principal_id": "operator:likesjx",
  "session_id": "telegram:7898847424:agent-jane-01",
  "hotel_id": "default",
  "channel_kind": "telegram",
  "action_class": "vault.secret.rotate",
  "action_target": "provider:gemini",
  "status": "active",
  "issued_at": 1741782000,
  "expires_at": 1741782300,
  "nonce": "random",
  "one_time_use": true
}
```

Recommended first semantic fields:

- `grant_id`
- `principal_id`
- `session_id`
- `hotel_id`
- `channel_kind`
- `action_class`
- optional `action_target`
- `status`
- `issued_at`
- `expires_at`
- `nonce`
- `one_time_use`

Later expansions can add:

- step-up auth state
- approving operator identity
- audit linkage
- cryptographic signature or detached proof material

### First grant lifecycle

Recommended first lifecycle:

1. session is elevated into `admin_elevated`
2. operator requests a dangerous action
3. hotel validates eligibility and issues a short-lived action grant
4. the secure admin flow presents the grant back to the hotel
5. hotel consumes the grant and performs the action
6. grant becomes `consumed`, `expired`, `revoked`, or `denied`

### First grant classes

Recommended first action classes:

- `vault.secret.add`
- `vault.secret.rotate`
- `vault.secret.revoke`
- `vault.status.inspect`
- later:
  - `perimeter.membership.invite`
  - `perimeter.membership.revoke`
  - `break_glass.recovery`

The point is not to create a giant permission ontology on day one.

The point is to stop dangerous admin actions from being indistinguishable from ordinary tool calls.

## Elevation Eligibility Recommendation

A session should become eligible for admin elevation only if policy says all of the following are true:

- the principal identity is allowlisted or otherwise trusted
- the channel/surface is approved for admin elevation
- the hotel permits admin elevation on that surface
- the focused role/incarnation is admin-capable when that matters

Additional rules worth pinning now:

- no ambient admin posture by default
- explicit elevation step required, such as `/admin`, TUI elevation, or an admin-scoped session entrypoint
- short TTL on elevated posture
- auto-expire on inactivity
- auto-expire on role switch or hotel handoff
- destructive or perimeter-changing actions may require step-up auth or second confirmation

## Why This Boundary Matters

This is how Philotic limits exposure to elevated permissions:

- the operator is the normal source of authority
- the session temporarily carries that authority
- the hotel enforces it
- the vault/control plane consumes narrow grants
- the agent does not become root just because it is participating in an admin conversation

Otherwise the system slides into the deeply ironic design where “the admin agent elevated itself because it was doing admin things.”

## Secret Administration Recommendation

Adding or rotating secrets should be treated as an admin/control-plane workflow, not as ordinary conversational tool use.

Recommended shape:

1. operator issues a high-trust action such as `/vault add gemini` or `/vault rotate telegram`
2. `membrane` verifies operator posture and opens a secure admin flow
3. a CLI/TUI or Mini App collects the action under explicit auth/approval
4. the hotel control plane performs the vault mutation
5. the result returns as structured admin output without surfacing raw secret material

The key rule is:

- `membrane` starts and brokers the flow
- the hotel control plane owns the mutation
- the vault owns the secret
- model-facing components never receive the admin key material

## Remote Admin Delegation

When an operator is connected to one hotel but the target action belongs to another hotel, the system should delegate the admin action to the owning hotel rather than trying to move secret authority around the mesh.

Recommended rule:

- admin entrypoint may be local to hotel A
- target vault action may belong to hotel B
- hotel A may broker the request
- hotel B must validate and execute the dangerous action locally
- the result should come back as structured admin outcome, not as secret material

This keeps secret ownership local while still allowing mesh-wide administration from one trusted operator surface.

### First delegation contract

The first remote admin delegation path should carry:

- source hotel
- target hotel
- principal id
- session id
- action grant id
- action class
- optional action target
- structured payload

This gives the owning hotel enough context to validate that the request is:

- coming from a trusted peer
- tied to a valid elevated session
- scoped to one dangerous action class
- still within grant lifetime

The result should come back as a structured admin outcome, not as an implicit success or a blob of human prose.

## First Slice Recommendation

Start with a deterministic graph manager that can:

- inspect agent profile/config
- inspect hotel manifests and live guests
- show routing/materialization state
- patch a bounded set of records with audit output
- initiate one high-trust vault admin flow without exposing raw secret material
- define and issue one explicit short-lived action grant for that flow
- keep that flow channel-agnostic so hotels without `membrane` use the same control-plane model through CLI/TUI
- define one explicit remote delegation envelope for actions owned by another hotel

Then wrap that same management plane in a TUI.
