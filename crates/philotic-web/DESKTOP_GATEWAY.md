# Desktop gateway hotel sessions

Status: implemented and locally integration-tested; not deployed. Public desktop
nginx remains deny-all pending coordinated website/gateway/hotel rollout.

## Authority

Website OAuth verifies provider identity. Existing Mongo users own admin roles;
the account-bound invitation is a separate admission requirement. The confidential
desktop gateway checks both via introspection, then requests a hotel session.
The hotel owns user records, identity links, session issuance and revocation.

`PHILOTIC_DESKTOP_GATEWAY_KEY` enables a **trusted identity attester**. It is a
separate server-only random credential, at least 32 characters, distinct from
the website's `DESKTOP_GATEWAY_KEY`. Possession allows the gateway to attest
administrators for this hotel; compromise requires immediate disable/rotation.
Do not put it in browser config, logs, cookies, or the public proxy. Private
transport must be loopback or an encrypted trusted overlay/TLS. This is a
transitional service credential, not end-user authentication.

## Contract

- POST `/internal/desktop/session`: configured gateway header, provider `google`
  or `github`, provider subject, matching hotel, random 43-character exchange ID,
  expiry no more than 15 minutes ahead; maximum body 4 KiB.
- One transaction consumes the exchange ID and inserts/links a stable distinct
  hotel user plus its session. No root fallback or email-based account merge.
  Conflicting legacy identity links fail closed and require explicit migration.
- GET `/internal/desktop/session/status`: requires hotel token plus gateway
  credential. Disabled users, expired/revoked sessions and missing configuration
  fail closed. Every desktop session lookup requires the gateway credential.
- Gateway keeps website and hotel tokens server-side. Browser cookies are opaque
  handles; caller bearer/cookie/identity headers are discarded.
- Request and WebSocket checks revalidate website admission/admin status and hotel
  session status. Idle sockets are checked every five seconds; request timeouts
  add bounded detection latency. Logout revokes both sessions; local access is
  removed even if remote revocation fails. Hotel expiry bounds crash orphans.
- Browser proxy excludes `/internal/`, device enrollment, bootstrap, challenges
  and legacy OIDC. No alternative session-issuance route through the gateway.

Existing admin capabilities and dangerous-action confirmations remain in force.
Distinct identities are not a claim of fine-grained per-resource tenancy.

## Verification

`cargo test -p philotic-web --bin philotic-web` covers hotel identity, expiry,
replay, disabled principal, gateway-token binding and legacy identity conflict.

Run the cross-repository test explicitly:

```sh
DESKTOP_GATEWAY_SMOKE_SCRIPT=/absolute/path/to/jaredlikes-desktop/tests/hotel-session-integration.mjs \
  cargo test -p philotic-web desktop_real_gateway_integration -- --ignored
```

It runs the actual Node gateway/client against Rust issuance, status and logout
handlers and temporary SQLite. Website identity is a controlled fixture, not real
OAuth or installed-runtime proof. It checks credential stripping, direct-token
denial, hidden issuance routes and cross-service revocation.

## Rollout gate

September 22 integration checkpoint: current develop (`6f0aceaa`, including
Cortex PRs #578/#579) is combined with the desktop bridge. All 199 standard web
tests, the explicit real Node-to-Rust integration test, and 17 desktop gateway
tests pass locally. Cortex retains the same gateway-bound session resolution.
This is not a deployment or a real OAuth/Mongo administrator-login proof.

The previously staged bridge-only binary must not replace the deployed Cortex
binary. Build one combined artifact, preserve the full desktop assets, and merge
website/desktop changes into their normal release branches before rollout so
automatic deployments cannot silently remove the new authentication endpoints.

Provision distinct server secrets securely, inspect pre-existing identity mappings,
deploy compatible website/gateway/hotel artifacts privately, verify real invited
admin login and non-admin denial, then replace public deny-all routing. Rollback
must remain deny-all. Never weaken checks to reuse a legacy root mapping or token.
