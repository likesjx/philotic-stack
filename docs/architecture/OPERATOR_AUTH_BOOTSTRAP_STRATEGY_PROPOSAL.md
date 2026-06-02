---
title: Operator Auth Bootstrap Strategy Proposal
doc_type: proposal
domain: operator-control-plane
status: accepted for current slice
last_updated: 2026-05-16
tags:
- operator
- auth
- oidc
- membrane
- desktop
- active-seam
related_docs:
- HOTEL_USER_IDENTITY_AND_OPERATOR_AUTH_PROPOSAL.md
- DESKTOP_MEMBRANE_PROPOSAL.md
- CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md
- ARCHITECTURE_STATUS.md
- ../process/OPERATOR_AUTH_ONBOARDING.md
task_refs:
- docs/task.md
proposal_id: operator-auth-bootstrap-strategy
implements: []
implemented_by: []
active_seams:
- operator-auth-bootstrap-strategy
- membrane-assisted-auth-challenge
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Operator Auth Bootstrap Strategy Proposal

## Goal

Define one explicit bootstrap strategy for desktop operator auth so Philotic stops oscillating between startup tokens, future-passkey dreams, and whichever membrane happens to be nearby holding a plausible story about the user.

This proposal answers:

- which auth path should be primary now
- how membranes may assist without becoming auth authorities
- how single-use challenge redemption should work
- where OIDC, membrane verification, and passkeys sit in the sequence

## Disposition

`accepted for current slice`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Core Recommendation

Use a three-layer strategy:

1. **OIDC primary**
2. **membrane-assisted single-use challenge for step-up and recovery**
3. **passkeys next**

Do **not** make philotes or membranes the issuer of operator sessions.

The hotel remains the only authority that may:

- validate a completed auth proof
- consume an auth challenge
- mint an `OperatorSessionRecord`
- revoke that session later

Membranes may help verify the human. They may not quietly become the front desk just because they know your Telegram handle.

## Recommended Order

### 1. OIDC primary

The first durable human login path should be OIDC-backed, using the public operator ingress on `brain.jaredlikes.com`.

Current preferred providers:

- Google
- GitHub
- Apple later, when added

Why lead with OIDC:

- real user identity exists already
- the web-facing setup work is largely done
- it avoids inventing a password system nobody actually wants to maintain
- it gives the always-on desktop on `vps-jane` a trustworthy first factor quickly

Loopback/local membranes are the intentional exception:

- local operator surfaces should prefer the hotel-issued bootstrap/back-door path by default
- localhost OIDC should require explicit opt-in configuration rather than silently deriving loopback callbacks from request headers
- public ingress is where OIDC belongs by default; local ingress is where bootstrap convenience belongs by default

### 2. Membrane-assisted single-use challenge

Trusted membranes such as Telegram may assist with a short-lived, single-use operator auth challenge.

That ceremony should be allowed for:

- step-up approval
- recovery
- personal bootstrap in trusted environments

It should not become the sole permanent identity root unless it grows stronger proof semantics than “a message came from a place we usually like.”

### 3. Passkeys next

Passkeys are the stronger long-term local-first auth posture and should remain the next major upgrade path.

They are not required before escaping bootstrap-token land, because OIDC already gives us a sane first move instead of a bespoke cryptographic hobby project.

## Membrane-Assisted Auth Rule

Membrane-assisted auth must be **hotel-authoritative**.

Correct shape:

1. desktop requests an auth challenge from the hotel
2. hotel creates a single-use challenge record
3. membrane helps verify the human and returns proof tied to that challenge
4. hotel validates the proof
5. hotel consumes the challenge atomically
6. hotel issues a bounded operator session

Incorrect shape:

- philote mints a login token
- membrane invents a durable session on its own
- challenge proof can be replayed
- one membrane approval implicitly authenticates every desktop forever

## Challenge Contract

The first challenge family should be explicit and hotel-local.

Suggested record:

### `OperatorAuthChallengeRecord`

- `challenge_id`
- `user_id`
- `intended_surface`
- `auth_path`
- `verifier_kind`
- `verifier_hint`
- `bind_label`
- `challenge_nonce`
- `issued_at`
- `expires_at`
- `status`
- `consumed_at`

Required rules:

- short TTL
- single-use only
- consumed atomically
- bound to one requesting desktop/surface
- replay-proof
- auditable

## OIDC Recommendation

OIDC should be modeled as:

- external identity proof
- hotel-validated callback result
- hotel-issued `OperatorSessionRecord`

The hotel should persist linkage metadata locally, but the session itself still comes from hotel authority rather than directly from the provider.

Provider `subject` should be the canonical external identity match key. Email and login are helpful aliases, not a safe root identity key to build the whole user graph on top of.

That keeps the external identity provider useful without letting it quietly replace the hotel as the system that knows who is actually operating the mesh right now.

## Always-On Desktop Recommendation

For the secure long-running desktop on `vps-jane`:

- pre-auth: show only the locked shell and bootstrap settings
- first factor: prefer OIDC
- step-up / recovery: allow membrane-assisted single-use challenge
- later hardening: add passkey support

This gives the operator continuity without turning the desktop ingress into a permanent ambient bearer shrine with uptime statistics.

## Current Slice

This slice accepts and implements the first groundwork seam:

- OIDC is now the preferred primary login path in architecture truth
- membrane-assisted single-use challenge is now the accepted step-up/recovery model
- `philotic-web` now persists hotel-local `operator_auth_challenges`
- `POST /api/auth/challenges` now issues a bounded pending challenge record with nonce, verifier metadata, and TTL
- `POST /api/auth/oidc/start` now issues a provider-bound OIDC challenge plus PKCE verifier and returns a provider authorization URL
- `GET /auth/oidc/:provider/callback` now exchanges the authorization code, fetches provider identity, consumes the hotel-local challenge, and issues the hotel-owned operator session cookie
- OIDC provider settings are now intended to live in hotel config truth: public callback base URL and provider client IDs in operator config, provider client secrets as vault-backed `*_secret_ref` config entries, with env-based settings retained only as transitional fallback
- successful OIDC callbacks now persist the first hotel-local `external_identity_links` records keyed by provider subject, with email/login retained as supporting aliases

This is intentionally **not** yet:

- Telegram proof verification
- passkey support
- mesh-visible operator identity projection

## Follow-On Slices

1. define trusted membrane proof shape for Telegram-assisted auth
2. consume verified membrane challenges into bounded operator sessions
3. expand provider/claim normalization beyond the first persisted subject/email/login slice and map those links onto richer local-first onboarding
4. add passkey-backed login as a stronger local-first auth factor
5. project non-secret operator identity/audit metadata across the mesh

## Open Questions

1. Should Telegram-assisted auth be allowed as primary bootstrap for a single-user personal mesh, or only as step-up/recovery?
2. Which OIDC claims should become mesh-visible identity projection versus remaining hotel-local?
3. How should hotel-to-hotel operator attribution work when one authenticated desktop is administering a different target hotel?

## Reality Gap

Current repo truth now has:

- hotel-owned operator sessions
- hotel-local root-user key refs
- a real `operator_auth_challenges` record family and issuance path

Current repo truth does **not** yet have:

- OIDC callback validation
- membrane proof verification
- passkey ceremony
- challenge redemption into a session

That is the correct kind of incomplete: the authority boundary exists first, and the fancy proofs can arrive without smuggling in a second auth system by accident.
