---
title: Personal MCP OAuth recall boundary
doc_type: proposal
domain: operator-control-plane
status: accepted-current-slice
last_updated: 2026-10-05
tags: [mcp, oauth, percival, lifegraph, muninn]
related_docs:
- AGENT_FRONTDOOR_PROPOSAL.md
- NATIVE_OPERATOR_SIGNIN_PROPOSAL.md
- ARCHITECTURE_STATUS.md
task_refs:
- docs/task.md#personal-mcp-oauth-recall-boundary
proposal_id: personal-mcp-oauth
active_seams: [personal-mcp-oauth-resource, personal-mcp-issuer]
---

# Personal MCP OAuth recall boundary

## Goal

Connect Percival's personal plugin to authoritative personal context without
giving it engineering coordination, graph-promotion or management authority.

## Core Recommendation

Use the first-party narrow OAuth handler in the existing identity deployment
and a recall-only protected-resource boundary over existing frontdoors. Keep existing native/desktop application login credentials
separate. First tools: `muninn_recall` and `life.recall`, with explicit independent
scopes and per-tool backend grants. User access tokens are never passed upstream.

## Disposition

Accepted local source slice; not deployed, connected, or approved for persistent
access. `services/personal-mcp` implements protected-resource discovery, issuer
readiness validation, confidential introspection, exact audience/operator/client
checks, bounded lifetime, revocation rechecks and fail-closed tool projection.

This is not an OAuth authorization server. The issuer must own user consent,
S256 authorization-code exchange, exact callbacks, registration, replay prevention
and refresh/revocation. The separately isolated identity worktree now implements
that handler using existing Google/GitHub sign-in and MongoDB. The production
handler is disabled by default; real ChatGPT/browser compatibility remains
unverified. No credentials, admission records or live routes were configured.

## Current Slice

43 synthetic Node tests pass, including real ephemeral HTTP request/response
tests. PKCE, callbacks and consent test the fake issuer; only requests through
the actual gateway establish gateway paths under synthetic upstream contracts. No actual issuer, personal data, credentials, backend grants, or deployment
was used. No Rust code changed. Source is published in draft PR [#616](https://github.com/likesjx/philotic-stack/pull/616), paired with identity [#9](https://github.com/likesjx/jaredlikes-com/pull/9). Neither is merged or deployed.

The adapter's direct outbound requests are confined to pinned issuer endpoints
and explicit recall backend URLs; no user-supplied URL or general HTTP tool exists.
It is a transitional perimeter adapter pending deployment and outbound-inventory
review, not a new hotel authority or general-purpose HTTP execution lane.

See [implementation and deployment gates](../../services/personal-mcp/README.md).
Claude's October 5 `b43ce808` on cached `origin/claude/stoic-goldberg-f8w99b`
plans frontdoor expiry and trusted caller tags. Reuse those plans for eventual
proposed LifeGraph evidence and Muninn contributions; neither write tool is in
this initial projection. No duplicate memory store is introduced.

## Issuer and placement review

The owner selected first-party OAuth on 2026-10-05. Identity worktree:
`/Users/jaredlikes/code/philotic-native-identity-percival-oauth`, branch
`codex/percival-first-party-oauth`. See its `docs/PERSONAL_OAUTH.md` for boundaries,
setup gates and future Apple sign-in. Stable internal subjects are separate from
provider identities; no account linking by email/private relay. Existing cookies,
recent sign-in and fresh Mongo role checks are reused; OAuth codes/access/refresh
use separate collections, exact bindings, atomic consumption/rotation and version
revocation. Website logout uses a protected POST and invalidates the subject's OAuth families;
trusted disable/re-enable operations atomically increment admission versions.

Validation: 56 identity tests pass with isolated Mongo and actual gateway
interoperability subtest; 43 original gateway tests pass. Typecheck and production
identity build pass. These are synthetic local tests, not real provider/browser/
ChatGPT acceptance. Keycloak is an alternative, not a requirement.

Review fixes support exact issuer identifiers, preregistration and RFC 8414
defaults; notification version checks, curated bounded recall arguments, explicit
vault configuration and safe backend errors are covered. Discovery and recently
verified callers have admission independent of unknown-token pressure. Trusted
edge rate/connection limits are a deployment acceptance blocker: new, rotated
and idle clients are not protected by recently verified token lanes. Revocation checks are uncached
at the gateway but are not atomic with response delivery or issuer replicas.

The Node adapter duplicates MCP transport handling. It remains a local
transitional contract implementation; production should prefer folding its
OAuth resource checks into an opt-in membrane-mcp auth mode, reusing native
endpoint grants, quotas, perimeter and protocol paths. No such migration has
been implemented or deployed.
