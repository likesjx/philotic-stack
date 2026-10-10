# Remote dot and Claude synthetic milestone

Source milestone only, 2026-10-10. No rollout, private-memory retrieval, consent,
credential provisioning, grant creation, security change or Claude contact occurred.

The gateway requires per-client policies when more than one client is allowed.
Policies bind each client to explicitly selected subjects, only `memory:recall`,
an exact single audience and `percival_connection_test`. Tokens are introspected
on every request and again before releasing recall output. An issuer grant-version
change invalidates one client's pending codes, refresh and access tokens without
invalidating the other client. Operations credentials are never memory credentials.

Example nonsecret onboarding configuration (synthetic identities, not real registrations):

```json
{
  "allowedSubjects": ["synthetic-operator"],
  "allowedClients": ["dot", "claude"],
  "muninnVault": "percival_connection_test",
  "enabledTools": ["muninn_recall"],
  "clientPolicies": [
    {"clientId":"dot","subjects":["synthetic-operator"],"vault":"percival_connection_test","scopes":["memory:recall"]},
    {"clientId":"claude","subjects":["synthetic-operator"],"vault":"percival_connection_test","scopes":["memory:recall"]}
  ]
}
```

Privacy is fail-closed by excluding private vaults from this client profile,
rejecting unapproved result shapes, dropping unapproved metadata and withholding
results on failed reauthorization. There is **no verified record-level privacy
filter for private memory**. Synthetic text is not inspected for semantic privacy.
Keep synthetic-only upstream credential scope as a separate operator acceptance
gate: the gateway cannot prove a backend honored the selected vault. No learning
or writes are exposed; native recall receives `read_only: true`. Semantic dedup
is not implemented. LifeGraph is not verified, enabled or advertised by this profile.

## Source and readiness reconciliation

`READINESS_DELTA.md`, `LINUX_FULL_HOTEL_ACCEPTANCE.md`, and the README describe
historical synthetic source/hotel evidence, not an installed production rollout.
The gateway source still contains a legacy optional LifeGraph adapter; this remote
profile explicitly rejects it. The old single-client identity patch is superseded
for this milestone by the site draft stacked on identity PR #10, which remains
unmerged. Reported x86 acceptance run 37986635372 succeeded historically; it was
not rerun or reinterpreted as real systemd acceptance here. Real systemd FD,
supervisor and isolation acceptance remains pending. UID991 is occupied; determine
an available numeric UID on the actual target before provisioning anything.
Reported live gateway `release-f12b1b46` and identity `oldmount-20261005` are prior
observations, not reverified runtime evidence in this task. Hardened rollout stays
disabled by default. No generic ops/release workflows are changed by this work.

## Acceptance and exact operator handoffs

1. Review both draft PRs and the dependency on unmerged site PR #10. Require
   hosted credential-free PR verification, including isolated Mongo and pinned
   issuer/gateway interoperability. Do not merge or deploy under this task's approval.
2. Obtain each client's actual supported OAuth mode and **exact** callback URLs
   from its owner; synthetic `.example` fixtures are not deployable callbacks.
   This preregistered public-client S256 implementation has no dynamic registration.
   Confirm clients support resource indicators and the gateway's unary JSON MCP
   transport; real dot/Claude interoperability has not been exercised.
3. Separately approve registration/configuration, introspection verifier/secret
   handling and explicit subject plus per-client versioned grants. No helper
   creates grants automatically; re-enable must increment the grant version.
4. Separately approve a synthetic-only scoped backend credential and test vault.
   Verify fixed broker roles, credential isolation, numeric UID, real systemd
   socket FD ownership, supervised binary identity and legacy rollback on target.
5. Under a new rollout approval, use synthetic packets only to verify discovery,
   PKCE, both client flows, callbacks, negative cross-client code/refresh/revoke
   attempts, revocation during recall, no writes/learning and independent revocation.
   Keep operations authorization separate. Private memory needs a separately
   reviewed privacy filter and authorization milestone. Parent coordinates Claude.

Local evidence: `npm test` in `services/personal-mcp`: 80 tests passed; site policy
and pinned gateway interoperability: 35 tests passed using installed Node24 and a
temporary TypeScript loader, no package installation. Full site typecheck and Mongo
integration require hosted CI. This is synthetic test evidence, not live-green.
