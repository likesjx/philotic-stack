# Personal MCP recall gateway

Local source slice for Percival; not deployed or connected. Node 22+; no npm
dependencies or install step. Test with `npm test` and check
syntax with `npm run check`. Tests use synthetic identities/content and ephemeral
loopback listeners only.

The gateway reuses the existing `membrane-mcp` tool contracts. It serves only
`muninn_recall` (`memory:recall`) and `life.recall` (`life:recall`) at the exact
configured resource path, with protected-resource metadata at the root and
path-specific well-known URI. Unauthenticated requests receive the discovery
challenge. Tools are both filtered and checked before dispatch. Recall is forced
to `read_only: true` for Muninn. No write/admin/graph/session tool can be dispatched.
Muninn reads are pinned to the configured vault (`default` in the synthetic
example); client-supplied other vaults are denied. Choose the intended personal
vault explicitly before deployment. Startup has no implicit vault default.
Recall arguments use a curated bounded schema, reject unknown fields, and cap
result counts/context sizes. Backend tool errors are replaced with a generic
error; malformed response shapes and raw diagnostics are not returned. Successful
results are reconstructed from validated text content and optional boolean
`isError`; unknown fields, text-item extras, `structuredContent` and `_meta` are
discarded because this recall projection defines no approved schema for them.

OAuth user tokens never reach backend endpoints. Each backend needs its own
operator-provisioned **recall-only** grant; do not reuse the 16-tool cloud-agent
grant. The service binds only `127.0.0.1`; production HTTPS and host routing remain
operator-owned. No service/supervisor or public route is installed by this slice.

## OAuth authority boundary

Use an established OAuth issuer. Existing desktop/native identity code does not
provide general MCP OAuth authorization. This module does not issue user tokens
or authorization codes, authenticate users, register clients, or implement a new
consent screen. Issuer discovery must advertise authorization-code flow and S256
PKCE (omitted `grant_types_supported` uses the RFC 8414 default). Explicitly
allowlisted preregistered clients need no CIMD or DCR advertisement. Issuer
identifiers are compared exactly, including trailing slash. Client registration
and the selected plugin builder mode still require acceptance testing.

The issuer must support confidential RFC 7662 introspection. Its response must
include `active`, `iss`, `aud`, `sub`, `client_id`, `iat`, `exp`, and `scope`. Audience
must match the exact configured MCP URL; the operator subject and plugin client
must be explicitly allowlisted. Lifetime is bounded to 15 minutes. Introspection
runs on every protected request and again before returning recall content. Outages deny
access, without diagnostics containing credentials or personal content.
This is a gateway-local no-cache check, not an atomic revocation guarantee:
issuer replicas/caches can delay visibility, and revocation can race the final
check and bytes sent. Already delivered data cannot be recalled. Measure the
selected issuer's actual revocation semantics and latency before connecting.

Discovery is served without entering the authorization concurrency pool. Missing
or malformed bearer headers are rejected before introspection. Unknown-token
requests have eight slots, separate from 24 slots for recently verified token
hashes. Thirty-second scheduling hints never grant authorization or skip checks.
This mitigates unknown-token floods after a valid client's first successful
request; it does not guarantee admission for a first-time client under attack.
The deployment edge must enforce per-source request rate/burst/body/header and
connection limits using its trusted proxy client address; limit discovery and
the issuer's own public endpoints too. No limiter or security setting was enabled
on the live edge by this slice.

Authorization-code replay protection, exact registered callback validation,
S256 verifier checks, browser CSRF/consent, resource binding at both authorization
and token exchange, refresh-token rotation and revocation belong to the issuer.
Discovery alone does not prove those features work: an actual issuer/client flow
and negative-path acceptance tests are required before deployment. Some issuers
omit introspection claims this adapter requires; verify compatibility rather than
weakening audience or operator checks. The gateway deliberately has no bearer
fallback to the native/desktop/admin credential classes.

Configuration is nonsecret JSON (see `config.example.json`); secret *environment
names* are references. `node server.mjs /path/to/config.json` is an explicit launch
command for a later approved deployment, not a step run during implementation.
The example uses reserved synthetic domains and cannot connect as supplied.

## Connection acceptance and approvals still needed

1. Select a compatible issuer and approve its operator identity/federation setup.
   Register only the exact redirect URI shown by the personal plugin builder.
   Verify real consent, denial, wrong callback, PKCE mismatch, code replay,
   audience mismatch, refresh replay and revocation using disposable identities.
2. Provision distinct recall-only backend grants and the confidential
   introspection credential in the operator's secret store. This creates live
   access and was not authorized/performed by this source slice.
3. Review/approve the VPS service and HTTPS route. Supply the exact resource,
   issuer endpoints, subject/client allowlists and secret environment references.
   Confirm TLS, request limits and loopback-only backend reachability.
4. Verify tools/list contains only the two recalls, each scope denial fails,
   writes/admin calls fail, issuer outage fails, and revocation denies the next
   call and in-flight response. Then connect the personal plugin.

The 43 tests include five protocol fixture tests. Their authorization-code,
PKCE, callback, consent and refresh assertions exercise the test-only issuer,
not a production issuer or gateway-owned authorization-server behavior. Requests
through the real gateway exercise its discovery, token validation, recall routing
and denial/revocation paths against synthetic upstream contracts. The fixture is test-only and must not be deployed; it does
not prove any real issuer's behavior, browser login, callback ownership, or TLS.
No Rust source changed; workspace CI and watched-live
deployment were not run.

## Adapter placement

This local service duplicates a small stateless MCP transport surface, not hotel
dispatch or data ownership. It deliberately stays outside the active Rust
membrane while the selected first-party issuer and deployment contract are being validated. Long term, move discovery,
issuer verification and scope projection into an opt-in `membrane-mcp` endpoint
auth mode, reusing its protocol, endpoint grants, perimeter, quotas and reply
handling. The separate gateway is transitional and should not become a second
permanent MCP authority. It must not be deployed just because its synthetic
tests pass. See [issuer/placement assessment](ISSUER_ASSESSMENT.md).

## Governed contributions after the recall pilot

Add separate scopes and separate per-tool backend grants for `life.observe`
(proposed evidence only) and `muninn_remember` (continuity only). Preserve source
IDs/timestamps/provenance, idempotent observation IDs, and the confirmed-truth
promotion gate. Neither graph promotion nor raw admin access should be granted.
Implement trusted server-side caller attribution before accepting contributions;
do not accept caller-supplied identity tags as authority. Coordinate with Claude's
October 5 `b43ce808` frontdoor F3 caller-tag plan and F1 task-expiry plan. Do not
automatically add either contribution tool to this recall gateway.

References: [OpenAI plugin auth](https://developers.openai.com/plugins/build/auth),
[MCP authorization](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization),
[frontdoor proposal](../../docs/architecture/AGENT_FRONTDOOR_PROPOSAL.md).

Deployment acceptance blocker: trusted edge rate, burst and connection controls
are mandatory. Recently verified token lanes only help warm tokens; new, rotated
or idle clients still compete with unknown tokens and can be denied under load.

## First-party issuer milestone (2026-10-05)

The owner selected a narrow handler in the existing identity app. Its isolated
worktree `philotic-native-identity-percival-oauth` documents configuration and
future Apple sign-in in `docs/PERSONAL_OAUTH.md`. The actual gateway accepts its
introspection contract in an isolated-Mongo interoperability test. Keep this
resource adapter temporarily to avoid expanding Rust route authentication and
IPC authority in the same change as the issuer. This is an explicitly tested
transitional boundary; native membrane integration remains proposed.
