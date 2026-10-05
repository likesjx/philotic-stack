# Issuer and placement assessment — 2026-10-05

Recommendation only; no accounts, providers, clients, grants or routes configured.

## Existing infrastructure

Local source shows Google/GitHub login through Arctic and an application-specific
MongoDB admission/session authority (`philotic-native-identity/src/lib/auth/oauth.ts`,
`docs/DESKTOP_ADMIN_AUTH.md`). Traefik fronts Philotic MCP on the VPS. No general
issuer deployment was found in the targeted local docs/config structure. This
does not exclude an undocumented service elsewhere. Google/GitHub identity login
can be brokered; their tokens are not Philotic custom-scope/audience tokens.

## Selected direction: first-party handler in existing identity app

Owner approved this revision on 2026-10-05. The separate identity worktree
`philotic-native-identity-percival-oauth` implements code/PKCE/consent,
confidential introspection, hashed tokens, durable Mongo atomic replay controls,
refresh rotation/family revocation and stable internal subjects. It reuses the
identity service and Mongo; no additional service/database is intrinsically
required. See that worktree's `docs/PERSONAL_OAUTH.md`. Future Apple sign-in is
planned through explicit verified identity mapping, without email-based linking.
57 isolated identity tests (including real Mongo/HTTP handlers and gateway
interoperability) and the original 43 gateway tests pass. Real ChatGPT linking,
TLS/browser/provider acceptance, review and deployment remain blocked. No live
setup occurred. This choice replaces the initial Keycloak recommendation below;
Keycloak remains a documented alternative, not a dependency.

## Alternative: Keycloak, conditional on a pinned-version acceptance run

Keycloak supplies confidential introspection, PKCE/code flow, DCR, refresh and
revocation. Its current MCP guide documents resource indicators as experimental
and a ChatGPT CIMD incompatibility in its documented implementation. OpenAI now
documents plural plus legacy singular CIMD metadata during transition, so actual
CIMD compatibility is version-dependent and unverified. Predefined OAuth clients
are a documented legitimate OpenAI path: prefer preregistration and verify that
the actual connection selects it; otherwise validate DCR. No CIMD workaround is
required for preregistration. [OpenAI auth](https://developers.openai.com/plugins/build/auth). Exact resource
processing needs the resource-indicators feature, rather than silently ignoring
`resource` and relying only on an audience mapper. These are readiness blockers,
not configurations this slice enabled. [MCP guide](https://www.keycloak.org/securing-apps/mcp-authz-server),
[endpoints](https://www.keycloak.org/securing-apps/oidc-layers).

It fits the existing Docker/TLS operating model but introduces a service and
relational database: MongoDB is not a supported Keycloak storage backend. Use a
supported database such as PostgreSQL, explicit container limits, backups and
upgrades. Official guidance recommends a 2 GB container memory limit for a small
production deployment. Existing VPS headroom and resulting hosting cost were not
measured. The Apache-2.0 software requires no subscription; hosting/operations
still cost capacity and time. [Database](https://www.keycloak.org/server/db),
[container sizing](https://www.keycloak.org/server/containers),
[license](https://github.com/keycloak/keycloak/blob/main/LICENSE.txt).

Owner decisions/access required:

- Approve self-hosted issuer operations, its domain/TLS, capacity, database,
  backup owner and pinned release; explicitly accept or reject experimental
  resource-indicator support. No package/image was installed or downloaded.
- Choose Google or GitHub brokering (or a separate issuer account). This requires
  provider-console access and a new exact broker callback/client registration;
  existing secrets were not read or reused. Keycloak supports both social
  brokers. [Identity brokering](https://www.keycloak.org/docs/latest/server_admin/index.html#_identity_broker).
- Approve the canonical MCP resource URL, exact plugin callback and client ID,
  intended personal Muninn vault, and single operator issuer subject. The example
  `default` vault is not a decision that it contains all intended personal memory.
- Decide whether plugin eligibility is independent explicit owner approval or
  must track website Mongo admission/revocation. Current adapter pins the issuer
  subject; website logout/admin-role removal is NOT automatically synchronized.
  The latter requirement needs a trusted identity mapping/admission adapter.
- Approve creating a confidential introspection client and distinct recall-only
  backend grants, plus edge request/connection limits and service routing.

Before claiming compatibility, verify returned introspection evidence includes
all required claims (`iss`, `aud`, `sub`, `client_id`, `iat`, `exp`, `scope`, active).
If provider output differs, use a reviewed provider-specific normalization based
on verified equivalent issuer evidence; never guess missing audience/client/user
claims or trust client request fields. Verify both resource parameters, callbacks,
consent, replay, refresh rotation and actual revocation behavior on the pinned
release. No actual Keycloak acceptance has been performed.

## Meaningful managed alternative: Auth0

Auth0 avoids self-hosted issuer/database operations and is named by OpenAI's MCP
auth guide. Its documented custom-API validation path is JWT signature/audience/
scope validation. It is not a drop-in match for this introspection adapter; verify
the tenant's exact supported contract and implement a separate established JWT
validator plus an explicit revocation policy if selected. Do not weaken issuer
or operator checks to make it fit. Owner needs tenant/Google-or-GitHub console
access and a plan/feature quote; no price or existing account is assumed here.
[Auth0 validation](https://auth0.com/docs/secure/tokens/access-tokens/validate-access-tokens),
[OpenAI auth](https://developers.openai.com/plugins/build/auth).

## Gateway/proxy duplication

The separate Node service repeats initialization, JSON-RPC dispatch, discovery,
tool listing, request limits and protocol version support already partly owned
by Rust `membrane-mcp`. It adds two backend grant dependencies and an extra hop.
That is useful for isolated contract tests without changing the active runtime,
but it is not the preferred permanent topology. Existing membrane routing,
upstream grants, allotments and perimeter remain authoritative. Do not turn the
Node service into a general proxy or independent tool registry.

Preferred production direction: an opt-in OAuth-resource auth mode in a dedicated
`membrane-mcp` endpoint, with pinned issuer configuration, async introspection,
subject/client binding, exact audience and per-tool OAuth scope checks alongside
existing route grants. Reuse native protocol/reply paths and existing quotas;
keep bearer endpoints separate. Preserve the gateway tests as contract tests.
This migration is proposed, not implemented or permission to change live auth.
