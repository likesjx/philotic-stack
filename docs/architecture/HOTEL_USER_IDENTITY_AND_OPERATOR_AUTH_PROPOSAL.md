---
title: Hotel User Identity And Operator Auth Proposal
doc_type: proposal
domain: operator-control-plane
status: accepted for current slice
last_updated: 2026-05-16
tags:
- operator
- identity
- auth
- hotel
- desktop
- root-user
- active-seam
related_docs:
- ARCHITECTURE_STATUS.md
- CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md
- DESKTOP_MEMBRANE_PROPOSAL.md
- OPERATOR_AUTH_BOOTSTRAP_STRATEGY_PROPOSAL.md
- ROLE_POSTURE_AND_ADMIN_PROPOSAL.md
- HOTEL_PERIMETER_TRUST_PROPOSAL.md
- KEY_VAULT_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: hotel-user-identity-and-operator-auth
implements: []
implemented_by: []
active_seams:
- hotel-user-authority
- operator-session-auth
- philote-user-projection
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Hotel User Identity And Operator Auth Proposal

## Goal

Define a hotel-owned user identity and operator authentication model so Philotic stops smearing operator truth across bearer tokens, desktop process state, and whichever role happened to be holding the scary tools at the time.

This proposal answers:

- where user identity lives
- where root user information and key references live
- how desktop/operator sessions authenticate
- how a long-running secure desktop server stays continuously reachable without becoming ambient authority
- what philotes are allowed to know about the user
- what should project across the mesh versus remain hotel-local

## Disposition

`accepted for current slice`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Core Recommendation

User identity should be **hotel-authoritative**.

The hotel, not the desktop membrane and not an ambient admin role, should own:

1. the canonical local user record
2. the local root-user key references and secret bindings
3. operator authentication and session issuance
4. principal posture and eligibility for elevation
5. the projected user context that philotes may consume

The desktop membrane should authenticate *to the hotel* and derive its operator session from hotel authority.

Nothing privileged should render before that authentication succeeds.

The always-on desktop server case does not weaken this rule. It strengthens it.

Philotes should receive a **projected user context**, not raw root-user secret material.

The mesh should carry a **ghost mirror projection** of user identity records that are needed for routing, audit, and cross-hotel operator continuity, while private key material and local secret bindings remain hotel-local.

Identity authority lives in the hotel graph. Identity understanding lives in the agent graph.

## Why This Boundary Matters

Without a hotel-owned user authority:

- the desktop becomes an accidental auth server with better CSS
- admin posture gets confused with identity
- philotes can start learning about the user by proximity instead of policy
- cross-hotel actions cannot be audited cleanly to a principal
- local root-user keys drift into whatever process currently feels important

That is efficient right up until the operator asks who actually approved a dangerous action and the answer is “somewhere between the membrane and vibes.”

## Canonical Split

### Hotel-local canonical truth

The hotel should own these records canonically:

- `UserRecord`
- `ExternalIdentityLinkRecord`
- `OperatorSessionRecord`
- `OperatorPostureRecord`
- `ActionGrantRecord`
- `RootUserKeyRefRecord`
- local vault bindings for user-scoped secrets and key material

These are hotel-local because they govern local authentication, local vault access, and dangerous action execution.

`ExternalIdentityLinkRecord` is where Google / GitHub / Apple linkage belongs. It is not agent-memory and it is not mesh-global secret material.

### Mesh-projected shared truth

The mesh should ghost-mirror only the subset of user/auth truth needed for distributed operation:

- principal identifier
- display identity
- public key or verification material
- posture eligibility metadata
- originating/home hotel
- cross-hotel operator session linkage metadata
- audit attribution fields

These records are for routing, trust, and attribution. They are not a license to replicate the user's raw root keys across the organism.

### Philote-visible projected truth

Philotes should receive only a bounded user projection such as:

- stable user id
- display name / preference surface
- authority/home hotel
- approved posture hints
- scoped identity traits relevant to the current task

Philotes should not receive:

- raw root-user private keys
- vault master keys
- reusable operator session credentials
- plaintext secrets just because they asked nicely in a new role

### Agent-graph versus hotel-graph split

The hotel graph should own:

- canonical local user identity
- onboarding state
- linked external identities
- operator sessions
- auth challenges
- root-user key references
- security posture and trusted membranes/devices

The agent graph may own:

- the philote's relationship model for that user
- inferred interaction preferences
- task-specific working assumptions
- memory about prior collaboration

The agent graph must not become the authority for login identity. Charming anthropomorphism is not a security model.

## Root User Model

Each hotel should maintain one canonical **root user** record for the human operator it serves locally.

This does **not** mean:

- one omnipotent mesh-global secret blob
- or one desktop-owned profile with magical transitive rights

It means:

- the hotel knows who its local operator is
- the hotel stores root-user key references in vault-backed form
- the hotel can derive bounded operator sessions and grants from that root identity
- the hotel can project the non-secret parts of that identity into mesh-visible ghost mirror state

Suggested first record families:

### `UserRecord`

- `user_id`
- `display_name`
- `home_hotel`
- `public_identity_material`
- `status`
- `created_at`
- `updated_at`

### `RootUserKeyRefRecord`

- `user_id`
- `key_purpose`
- `vault_ref`
- `public_fingerprint`
- `rotation_state`
- `updated_at`

### `ExternalIdentityLinkRecord`

- `user_id`
- `provider`
- `provider_subject`
- `email`
- `login`
- `display_name`
- `verified_at`
- `last_seen_at`
- `created_at`
- `updated_at`

### `OperatorSessionRecord`

- `session_id`
- `user_id`
- `issuing_hotel`
- `surface_kind`
- `posture`
- `issued_at`
- `expires_at`
- `status`

## Current Slice

The first concrete bootstrap/session slice is now real in `philotic-web`:

- `operator_users`, `root_user_key_refs`, and `operator_sessions` tables are created in the hotel context DB
- `philotic-web serve` now generates a startup bootstrap token instead of auto-granting the desktop a live operator cookie on page load
- the desktop root route now always serves the embedded desktop shell, while unauthenticated operator UX is pushed into `System Settings > Aiua Membrane`
- `POST /api/auth/bootstrap` exchanges the bootstrap token for a bounded operator session cookie
- `GET /api/auth/status` reports whether the current browser session is authenticated so the desktop can stay locked until the hotel issues a session
- `POST /api/auth/logout` revokes the current operator session and clears the cookie
- the desktop shell now carries the first system-level lock gate: non-settings workspace apps are blocked and redirected into `System Settings > Aiua Membrane` until the hotel issues a session
- `root_user_key_refs` is now seeded as a real hotel-local projection: `philotic-web` records the local vault-root-key locator plus a non-secret fingerprint and exposes that metadata through auth status for the desktop/bootstrap surface

This is intentionally a first slice, not a final auth model:

- auth still uses a hotel-issued bootstrap token rather than passkey or local-device mediation
- root-user key refs are now materially populated, but only from the current key source inspection path (keychain/env), not yet from a richer login or step-up identity ceremony
- operator posture is still a simple `admin` session default rather than a richer elevation flow
- the next accepted bootstrap direction is now explicit in [OPERATOR_AUTH_BOOTSTRAP_STRATEGY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/OPERATOR_AUTH_BOOTSTRAP_STRATEGY_PROPOSAL.md): OIDC primary, membrane-assisted single-use challenge for step-up/recovery, passkeys later
- `philotic-web` now has a first provider-backed OIDC ceremony seam: provider discovery in auth status, `/api/auth/oidc/start` for PKCE-backed challenge issuance, and `/auth/oidc/:provider/callback` for code exchange and hotel-issued operator session issuance
- OIDC settings are now moving under hotel authority too: the intended canonical split is hotel-config-backed public base URL and provider client IDs plus vault-backed provider `*_secret_ref` config keys, with env values retained only as transitional fallback while operator config catches up
- the hotel auth store now persists first real `external_identity_links` records keyed by provider subject, so successful OIDC logins stop being display-name-only proof and start attaching durable Google/GitHub identity linkage to the canonical hotel-local operator user
- `GET /api/auth/status` now exposes that non-secret external identity linkage so the future `User Settings` surface has a real hotel-owned user graph seam to build on
- `philotic-web` now exposes a first bounded `GET/PATCH /api/auth/user` surface that reads and updates the canonical hotel-local operator user record, bridges timezone/display-name through the existing hotel profile seam, and gives local-first onboarding a real home before mesh projection or agent personalization join the party

## Desktop Auth Recommendation

The desktop membrane should authenticate against the hotel and receive a bounded operator session.

Recommended shape:

1. desktop membrane acquires its runtime lease from the hotel
2. operator authenticates locally against hotel-owned identity
3. hotel issues a bounded `OperatorSessionRecord`
4. before that session exists, the desktop may show only an unauthenticated bootstrap surface
5. after session issuance, the desktop uses that session for local reads and remote control-plane routing
6. elevated or dangerous actions still require target-scoped grants

This means the desktop is an operator surface, not the source of operator truth.

### No-View-Before-Auth Rule

The secure desktop surface should have a very boring, very strict rule:

- no hotel status
- no mesh target inventory
- no component inventory
- no agent inventory
- no config
- no secrets metadata
- no event log

until the operator is authenticated and a bounded operator session exists.

Allowed unauthenticated surface:

- identity bootstrap
- login / step-up ceremony
- session-expired notice
- basic surface health such as “desktop membrane reachable”
- desktop shell frame plus lock-state explanation

This keeps “always on” from quietly becoming “always visible.”

## Always-On Secure Desktop Server

An always-on operator desktop on `vps-jane` is a good idea, but only if it stays subordinate to hotel authority instead of becoming a permanent ambient god-mode browser with suspiciously good uptime.

Recommended shape:

1. `vps-jane` materializes a bounded always-on desktop membrane surface as an operator ingress
2. that surface authenticates against `vps-jane` hotel-owned user identity and session issuance
3. the membrane acquires a hotel-governed authority lease before exposing privileged routes
4. the membrane renders only an unauthenticated bootstrap shell until hotel auth succeeds
5. remote mesh administration still routes through target-hotel control-plane surfaces with attribution and confirmation
6. the always-on surface should prefer step-up-friendly operator login such as passkey or hotel-issued bootstrap ceremony rather than long-lived shared bearer material
5. root-user key refs remain vault-backed and hotel-local even when the operator desktop is reachable continuously

This means the always-on desktop is:

- a durable operator access point
- mesh-aware
- session-governed
- lease-governed
- revocable
- authentication-gated before any meaningful view model is revealed

It should not be:

- a second independent authority source
- a long-lived bearer-token shrine
- a place where remote-hotel secrets become plaintext by convenience
- a status dashboard that leaks topology or live state before login

The useful split is:

- `vps-jane` desktop surface provides continuity
- target hotels still own truth, secrets, and dangerous action execution

### Recommended Runtime Shape

The most plausible secure long-running shape is:

1. a long-running `philotic-web serve` or successor `membrane.desktop` process on `vps-jane`
2. bound to a stable ingress address
3. serving only a login/bootstrap shell until operator auth completes
4. issuing short-lived operator sessions from hotel authority
5. refreshing those sessions with explicit activity + lease renewal
6. failing closed to the login shell on lease loss, session expiry, or target-hotel denial

That means the desktop server is long-lived, but operator visibility is still short-lived and revocable.

## Relationship To Existing Admin Posture Work

This proposal complements, rather than replaces:

- [CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md)
- [ROLE_POSTURE_AND_ADMIN_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROLE_POSTURE_AND_ADMIN_PROPOSAL.md)
- [DESKTOP_MEMBRANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DESKTOP_MEMBRANE_PROPOSAL.md)

Split of responsibilities:

- this proposal answers **who the operator is and where that identity is anchored**
- admin posture proposals answer **what a valid operator session may do**
- desktop membrane proposals answer **how a local operator surface acquires and uses that session**

## Mesh Behavior Recommendation

Because the mesh is a single organism, hotels should ghost-mirror enough user identity and operator session metadata to support:

- cross-hotel audit attribution
- target-hotel authorization decisions
- role and philote transport with correct user context
- routing decisions that depend on user-home locality or trust

But the mesh should not replicate:

- raw root-user private keys
- hotel vault master keys
- plaintext secrets
- unconstrained session bearer material

Rule of thumb:

`user identity may be mesh-visible; user secrets remain hotel-local`

## Philote Projection Recommendation

Philotes should not fetch user authority directly.

Instead, the hotel should project a bounded user context package into:

- role activation
- active operator session context
- targeted task envelopes
- tool/skill policy decisions

That package should be sufficient for:

- personalization
- preference-aware reasoning
- accountability
- deciding whether the current role/session is operator-linked

It should be insufficient for:

- impersonating the operator cryptographically
- minting its own admin session
- bypassing hotel-issued grants

## First Slice

The first honest slice should be proposal and schema level:

1. define `UserRecord`, `RootUserKeyRefRecord`, and `OperatorSessionRecord`
2. define the canonical split between hotel-local and mesh-projected user/auth truth
3. define the first desktop auth flow as hotel-issued operator sessions
4. define the first philote-visible user projection contract
5. define the first secure always-on desktop-server posture for `vps-jane`
6. wire the proposal into current operator-control-plane docs and tasks

## Current Slice

Current accepted direction for this proposal is:

- hotel-authenticated desktop sessions should replace ambient/debug-token-era posture
- the always-on desktop server on `vps-jane` should exist as a durable ingress point
- the visible desktop must stay auth-gated until a hotel-issued operator session exists
- post-auth read models should come from hotel-owned projected surfaces, not browser-direct runtime reads
- dangerous actions remain confirmation/grant shaped even after login

## Follow-On Slices

1. persist hotel-owned user records and root-user key refs in graph + vault-backed form
2. add a first real OIDC login/bootstrap path for desktop auth
3. add hotel-local single-use auth challenges for membrane-assisted step-up/recovery
4. replace ambient desktop debug token assumptions with bounded operator sessions
5. gate all desktop read models behind authenticated operator session issuance
6. project bounded user context into philote/session activation
7. define cross-hotel operator session continuity and audit attribution
8. define multi-user support once the single-root-user model is stable

## Open Questions

1. Which OIDC provider should be enabled first on the hotel-owned callback path: GitHub, Google, or both together?
2. Should one hotel be the canonical home for a user while others hold mirrored operator identity projections, or should each hotel have equal local authority for the same user record?
3. Which parts of operator session state should be mesh-visible versus strictly target-hotel local?
4. How should role/philote transport preserve user linkage without smuggling reusable auth?
5. When multiple humans eventually use the same mesh, which records remain hotel-local and which become organization- or mesh-global?

## Reality Gap

Current repo truth has:

- desktop bearer-token shaped auth
- operator session posture as an active design seam
- target-scoped confirmations for dangerous actions

Current repo truth does **not** yet have:

- a first-class hotel-owned user record
- hotel-issued operator sessions as the normal desktop auth path
- root-user key references modeled as canonical hotel records
- a formal philote-visible user projection contract

That is the gap this proposal is meant to close next, before the operator surface quietly becomes the identity system by accident.
