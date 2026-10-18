# Whole-vault two-client activation preparation

Code preparation only. No credentials, admissions, proxy settings or deployment
are activated by this change. Native observe compatibility passed against the
public v0.11.0 binary in run39142737907. This change adds a separate gateway
configuration contract and a private operator transaction wrapper.

## Gateway configuration delta

Keep the existing issuer, broker and loopback upstream composition. Set only
`muninn_recall`, fixed `muninnVault`, singleton `allowedSubjects`, and the exact
clients `claude_muninn_recall_v1` / `chatgpt_muninn_recall_v1`. For each client,
`clientPolicies` must bind that same subject/vault and only `memory:recall`.
Opt in with `wholeVaultAccess: {version:1, subject:SUBJECT, vault:VAULT,
includeFutureRecords:true}`. This is operator configuration, not proof of consent.
Without this opt-in, the existing synthetic multi-client restriction remains.
The gateway requires exact audience/scope, fresh confidential introspection on
each request and again after recall, forces read_only, and rejects vault overrides
and write tools. Already delivered bytes cannot be withdrawn. This change does
not add issuer coordination, LifeGraph or PR38/677 requirements.

The minimal path shares one backend observe key for one approved vault. OAuth
clients remain separately revocable through canonical issuer admissions/families.
Multiple vaults, distinct backend keys per client, excluded future records, or
per-record privacy filtering require a different reviewed mapping; none is
silently implemented here. Whole-vault approval includes every record in scope,
including later additions/updates. Complete cognitive-write suppression is not
claimed.

## Private provisioning wrapper

`persistent-provisioning/manifest.unset.json` intentionally leaves vault,
future-record inclusion and expiry unset. The only CLI is plan-only:

```
python3 services/personal-mcp/persistent-provisioning/provision.py PATH_TO_MANIFEST
```

A completed manifest is SHA256-bound. The operator-controlled runner must call
`execute(manifest, journal, confirm, private_handoff)` and obtain the exact
`CREATE_AND_STORE <digest>` confirmation before invoking private authentication
or transmitting credentials. Do not call this from an agent tool with live
secrets. No credentials are accepted through argv/environment, collected by the
CLI, or printed. Use a private owner0700 nonsymlink journal directory.

The wrapper reuses the existing trusted API and encrypted-vault owner interfaces:
- API: request(method,path,payload,admin=True), close_private_session().
- Broker: store_exact_acl(kind,scope,secret,allowed_roles,allowed_guests), returning
  only a reference after durable store and exact ACL/readback verification.

The supplied adapters in tests are dummy fixtures. **A production private runner
and its authenticated API / persistent-scope broker adapters are not installed or
validated by this change.** The existing synthetic helper has a hardcoded synthetic
scope; do not pass it a real vault or claim it implements this adapter. Integrating
these existing owners into the private runner requires review and action-time
approval before any live credential transmission.

After confirmation, the wrapper checks vault-name metadata only, creates one
native observe key with exact expiry (maximum31days), stores it and a separately
generated confidential introspection credential with singleton gateway ACLs, and
records IDs/references/verifier in the private journal. It does not seed/read
records, change vault visibility, adopt references, register clients, enable
admissions, write issuer config or restart services. The verifier is not printed.
Every effect has durable pending intent; unknown results or interrupted writes
require reconciliation, never automatic repeat. Returned malformed key metadata
retains a validated key ID for operator recovery. A pending create with no ID
requires native key-metadata reconciliation before any retry.

## Minimal action-time manifest still required

- Exact vault/client mapping and inclusion of future additions/updates; currently unset.
- Explicit key expiry/rotation term; currently unset, wrapper caps at31days.
- Subject15393f50-045b-4227-90b0-0cf4264f0b3d; current admission metadata recheck.
- Confirm actual Claude callback https://claude.ai/api/mcp/auth_callback and obtain
  ChatGPT's exact callback; no registrations are made by this artifact.
- Issuer https://www.jaredlikes.com; resource https://mcp.jaredlikes.com/personal/mcp.
- Approve creation/storage of native observe and confidential introspection
  credentials, exact subject/client admissions, broker reference adoption and
  issuer configuration; record actual returned IDs and hashes before activation.
- Approve proxy allow-address correction from172.18.0.7 to rechecked Traefik
  address (172.18.0.2 on Oct18), retaining deny-all; validate then reload nginx.
- Approve scoped hotel restart if required for startup-loaded reference adoption,
  and gateway restart. Preserve deployed coordination and unrelated services.

Rollback: withdraw gateway to discovery-only, disable/version-bump exact client
admissions and revoke families, reconcile/revoke new native key IDs, delete only
owned new broker references through existing owner APIs, and restore captured
public config. Never roll admission versions backward or delete memory records.
Stop and retain the journal for unknown outcomes; expiry is not revocation proof.
