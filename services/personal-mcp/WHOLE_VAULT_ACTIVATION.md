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

## Private provisioning adapters

`persistent-provisioning/manifest.unset.json` intentionally leaves vault,
future-record inclusion and expiry unset. The default preparation command is non-mutating:

```
python3 -I -B services/personal-mcp/persistent-provisioning/provision.py plan --manifest PATH_TO_MANIFEST
```

A completed manifest is SHA256-bound, including the exact localhost REST/UI
ports and built helper SHA256. They remain unset until verified. Run `plan
--manifest PATH` to inspect the nonsecret transaction. The executable private
handoff is installed root-owned and non-writable by others under
`/opt/percival-personal-mcp/provisioning` with provision.py, adapters.py,
private_key_handoff.py and the built persistent-broker-helper. Installation and
actual execution remain action-time operations, not performed by this change.

In an operator-controlled VPS terminal (not an agent/tool invocation):

```
sudo /usr/bin/python3 -I -B /opt/percival-personal-mcp/provisioning/provision.py apply --manifest APPROVED_MANIFEST --journal PRIVATE_0700_DIRECTORY/receipt.json
```

Apply refuses noninteractive execution, non-Linux/non-root callers, mutable or
symlinked installation paths, and helper-hash mismatch. It disables core dumps
and dumpability before prompting. The user must type `CREATE_AND_STORE <digest>`
before hidden prompts for the existing Muninn admin login and existing hotel
master key. No secret discovery, environment reads, SSH copying or generation of
a replacement hotel master key occurs. Secret inputs stay in memory/private pipes;
there is no argv/env credential input. Prompt echo fallback is refused.

The concrete API adapter speaks actual Muninn login/session-cookie and admin-key
JSON contracts on literal localhost only, with redirects/proxies disabled and
bounded requests/responses. Only vault-name metadata and observe-key creation
are allowed. The concrete broker adapter invokes a separate persistent helper
entry point sharing the existing hotel `vault.rs` encryption/storage and
`private_start.rs` privilege-drop/undumpability handshake. It validates scope and
singleton ACLs, opens only the existing hotel database, stores, checks metadata
and decrypts for readback, then returns only the reference and exits. The master
key is delivered after READY and lives in a private no-swap tmpfs destroyed on
namespace exit. The launcher/helper share one killable process group; timeout
kills and waits without repeating creation. A killed launcher can leave an empty
root-only /run directory, but cannot preserve its namespace-mounted key file.

Broker records persist after helper exit and do not expire automatically. The
native observe key has its explicit expiry; encrypted references and the separate
introspection credential require explicit revocation/removal. The adapter never
changes the serving broker policy, admissions, proxy or service state.

Tests cover real HTTP cookies/key response shapes, actual subprocess pipe and
exit/timeout behavior, actual encrypted storage surviving a child-process exit,
wrong ACL rejection and the reused Linux protected-handoff adversary fixture.
Live operator execution against the installed DB and admin endpoints remains
unperformed. A successful CI build is not production provisioning acceptance.

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
