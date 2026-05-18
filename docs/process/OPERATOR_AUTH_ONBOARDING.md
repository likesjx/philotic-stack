---
title: Operator Auth Onboarding
doc_type: workflow
domain: workflow-docs
status: active
last_updated: 2026-05-17
tags:
  - auth
  - oidc
  - onboarding
  - desktop
  - operator
related_docs:
  - ../architecture/HOTEL_USER_IDENTITY_AND_OPERATOR_AUTH_PROPOSAL.md
  - ../architecture/OPERATOR_AUTH_BOOTSTRAP_STRATEGY_PROPOSAL.md
  - ../architecture/DESKTOP_MEMBRANE_PROPOSAL.md
  - WORKFLOW.md
task_refs:
  - ../task.md
---

# Operator Auth Onboarding

This guide explains how a human gets from "desktop is locked" to "operator identity is configured and durable."

It exists because the current auth stack has finally become real enough to be useful, and therefore real enough to become confusing if we keep explaining it through scattered chat fragments and hopeful memory.

## Core Rule

- **local operator surfaces** use the hotel-issued bootstrap/back-door path by default
- **public operator surfaces** use OIDC by default
- **the hotel** is the only authority that may issue the operator session
- **successful OIDC login** attaches external identity links to the hotel-local user graph

That means:

- `http://127.0.0.1:*` is primarily for bootstrap convenience and local setup
- `https://brain.jaredlikes.com` is the canonical public operator ingress for `vps-jane`

## What Gets Stored

The hotel-local auth store currently owns:

- `operator_users`
- `root_user_key_refs`
- `operator_sessions`
- `operator_auth_challenges`
- `external_identity_links`

What OIDC contributes:

- provider name (`google`, `github`, later `apple`)
- provider subject / stable external ID
- supporting aliases such as email or login
- a stable projected mesh principal like `user:google:<subject>` once the first provider link is attached
- a durable `ProjectedUserIdentitySync` ghost-mirror event so peer hotels can learn that non-secret projected identity without inheriting local sessions or vault bindings

What it does **not** do yet:

- create full mesh-wide user projection
- replace the hotel as the identity authority

## First Admin Setup

Use this when the hotel has no OIDC settings yet, or when you are bringing up a fresh local environment.

### 1. Unlock locally with the bootstrap token

1. Open the desktop membrane.
2. Open `System Settings > Aiua Membrane`.
3. Use the bootstrap token to start the first operator session.

Why:

- the bootstrap token is the intentional back-door for local setup
- it lets the first admin configure auth mechanisms without already having auth mechanisms, which is one of those circular dependencies that becomes hilarious only after the outage

### 2. Configure provider auth from inside the hotel

Still in `System Settings > Aiua Membrane`, fill in the OIDC hotel config:

- `oidc_public_base_url`
- `oidc_google_client_id`
- `oidc_google_client_secret_ref`
- `oidc_github_client_id`
- `oidc_github_client_secret_ref`

Rules:

- client IDs live in hotel config
- client secrets live in the hotel vault and are referenced by `*_secret_ref`
- do not paste raw client secrets into hotel config

### 3. Enrich the local user graph

In `User Settings`, fill in the canonical hotel-local operator record:

- `display_name`
- `preferred_name`
- `primary_email`
- `timezone`
- `onboarding_state`

This gives the local user graph a real human-owned record before OIDC starts enriching it with external identity links.

## Public OIDC Setup For `vps-jane`

For the always-on public operator surface:

- hotel name: `vps-jane`
- canonical public ingress: `https://brain.jaredlikes.com`

Expected OIDC callbacks:

- Google: `https://brain.jaredlikes.com/auth/oidc/google/callback`
- GitHub: `https://brain.jaredlikes.com/auth/oidc/github/callback`

Deployment rule:

- seed `oidc_public_base_url` through hotel config / Ansible
- do not rely on request-header-derived loopback URLs for production ingress

## Local Development Rule

Loopback membranes should usually **not** use OIDC by default.

If the public base URL is only derived from request headers like:

- `http://127.0.0.1:7700`
- `http://localhost:7701`

then the hotel should direct the operator to bootstrap/back-door auth instead of sending them into provider redirect mismatch bureaucracy.

OIDC on loopback is allowed only when explicitly configured on purpose.

## Bringing A New Operator In From Scratch

Use this flow when another human needs access and has not been configured yet.

### Option A: local-first onboarding

1. An existing admin unlocks the desktop locally with the bootstrap/back-door path.
2. The admin configures OIDC on the hotel if it is not already configured.
3. The new operator signs in through Google/GitHub on the appropriate surface.
4. The hotel issues the operator session.
5. The hotel records the external identity link.
6. The admin or operator fills in local `User Settings` to enrich the canonical hotel-local user record.

### Option B: public operator ingress

1. The new operator visits the public desktop surface.
2. They sign in through OIDC.
3. The hotel issues an operator session.
4. The hotel records external identity linkage.
5. Follow-up local user enrichment happens through `User Settings`.

## Recommended Human Mental Model

- bootstrap token = first-admin setup and local recovery tool
- OIDC = normal durable login path for public or shared operator access
- external identity links = how the hotel starts learning who the operator is
- user settings = where the canonical local user record becomes richer than the provider callback

## Current Gaps

Still intentionally incomplete:

- richer multi-user local creation/resolution instead of linking everything onto the current canonical root-user shape
- mesh-visible projected user identity
- philote-bounded user-context projection
- Apple auth linkage
- membrane-assisted single-use step-up/recovery ceremony

## Best Next Move

When onboarding another operator:

1. make sure the first admin can unlock locally with bootstrap
2. make sure OIDC is configured with hotel-owned config + vault-backed secret refs
3. make sure the operator completes `User Settings` after first login

That gets us out of bootstrap adolescence without pretending the local user graph has already graduated into full organism-wide identity adulthood.
