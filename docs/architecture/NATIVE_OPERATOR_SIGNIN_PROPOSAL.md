---
title: Native Apple Operator Sign-In
doc_type: proposal
domain: operator-control-plane
status: accepted-current-slice
last_updated: 2026-10-01
tags: [apple, authentication, pkce, cortex, gateway]
related_docs:
  - CORTEX_VIEWER_PROPOSAL.md
  - HOTEL_USER_IDENTITY_AND_OPERATOR_AUTH_PROPOSAL.md
  - ARCHITECTURE_STATUS.md
task_refs: [docs/task.md]
proposal_id: native-operator-signin
active_seams: [operator-session-auth]
source_of_truth_targets: [ARCHITECTURE_STATUS.md]
---

# Native Apple Operator Sign-In

## Goal

Open Cortex from the Apple apps without copying administrator tokens or giving
the app the desktop gateway's confidential credential.

## Core Recommendation

Use system-browser sign-in and a one-time S256 PKCE handoff. The native app is a
public client: PKCE proves possession of an attempt verifier, not app identity.
Follow [RFC 8252](https://www.rfc-editor.org/rfc/rfc8252.html); do not collect
Google/GitHub credentials in an embedded web view.

The website remains responsible for provider identity, Mongo-backed administrator
status and account-bound invitation admission. The gateway retains website and
hotel sessions server-side. The hotel remains the issuer/validator of hotel
authority. Existing browser cookies and gateway-bound hotel tokens are not
native credentials.

### Accepted contract — rollout pending

1. The app creates independent 256-bit verifier/state values. Only state and the
   S256 challenge enter the browser authorization request, never the verifier.
2. The browser completes the existing website sign-in/admission flow. Show the
   account, native client and Cortex-read-only request before confirmation;
   defend authorization and consent requests against CSRF and login swapping.
3. The server binds a short-lived code to that account, attempt, client ID,
   exact registered callback and S256 challenge. No arbitrary return URL.
4. The callback carries only code and state, or a bounded cancellation error.
   It never carries a bearer token, website session or hotel credential.
5. Redemption uses a pinned, agreed HTTPS origin and a POST body. Validate all
   bindings and freshly recheck admission/admin status. Consume the code
   atomically, once; a maximum 120-second lifetime is proposed.
6. Return an opaque native handle with its own audience/type and server-enforced
   Cortex-read-only scope. It must not work as a browser cookie, hotel session,
   device enrollment token or authorization for arbitrary gateway routes.
7. Each read rechecks website admission/admin and hotel revocation, including
   after a long read before returning its result. Failure is denial, not cached
   access. Logout revokes the handle and its owned backing sessions; logout of
   one native session must not revoke unrelated sessions.

No handles or verifier in URLs/logs. Redact short-lived codes in callback access
logs and disable caching/referrer leakage. Bound initiation/redemption rates and
pending attempts. No offline memory cache or refresh-token persistence in the
first implementation. Handle/verifier stay in memory and clear on cancel,
disconnect or background; server expiry bounds failed logout or process crashes.
The later browser-integration implementation must explicitly handle scene
transitions during system authentication without accidentally retaining secrets
after cancellation or returning into a cleared attempt.

## Disposition

Accepted for the current slice. The operator approved Google sign-in and
`https://desktop.jaredlikes.com` as the exchange and Cortex-read origin. Client
and gateway implementation are test-green; public routing and authenticated
physical-device proof remain pending. Approval of the origin does not authorize
an unrestricted listener or arbitrary gateway routes.

## Current Slice

The shared Cortex screen now uses `ASWebAuthenticationSession` and an ephemeral,
redirect-rejecting `NativeCortexClient`. The app starts `/native-auth/start`,
redeems the one-time code at `/native-auth/exchange`, reads only `/native/cortex`,
and revokes at `/native-auth/logout`. Native handles have the distinct
`native-cortex-` prefix, `cortex:read` scope and at most 15-minute lifetime.
The verifier/state attempt lasts five minutes; the gateway code lasts at most
120 seconds. Credentials and Cortex contents remain memory-only.

The iOS callback scheme is registered as
`com.philotic.apple.ios:/oauth/callback`. Generation checks prevent cancellation
or cleanup of an old attempt from destroying a newer sign-in. Browser callbacks
carry code/state only, never credentials. Mac physical sign-in remains unproven.

Gateway source is isolated in `codex/native-cortex-signin`; website display and
asset isolation are in `codex/native-cortex-identity`. Website admission and
Mongo administrator status remain separate gates; invitations never grant roles.
The gateway still binds loopback. Deployment must preserve the existing desktop
and publish only approved native/callback routes, not general `/api/` or `/ws`.

## Verification and Remaining Gates

Local tests cover the RFC 7636 challenge vector, independent entropy, both client
callbacks, replay, expiry, cancellation, state mismatch, wrong origin/client,
duplicate parameters, fragment injection and encoded-path aliases.

October 1 verification: 115 Swift tests executed, one skipped, zero failures;
the generic physical-iOS build and strict signature verification passed. Website
typecheck, isolated-asset production build and 13 auth tests passed (one Mongo
integration test skipped). Gateway restoration passed 18 tests on September 30,
including real loopback HTTP reads, audience refusal and mid-read revocation.
The paired iPhone is currently unavailable; this is not installation or live
Google-to-Cortex proof. Public route deployment, account-bound admission and
real authenticated device reads remain required.

Before enabling real sign-in, test server-side concurrent redemption, replay,
expired/mismatched codes, wrong verifier/account/callback, audience and scope
confusion, non-admin/uninvited/disabled users, revocation during reads, secret
redaction, cancellation/background lifecycle and logout failures. Then prove
real Mac sign-in and memory reads, followed by physical iPhone installation and
the same flow. These are not established by local primitive tests.

See [execution work](../task.md) and
[Cortex verification](CORTEX_VIEWER_PROPOSAL.md).
