# Scoped vault integration candidate — 2026-10-08

Current base: develop 45b6aef9. Isolated branch codex/percival-scoped-integration.
No live configuration, account, credential, supervisor or service was changed.

## Implemented source

service/mod.rs declares scoped_vault, scoped_vault_resolver and scoped_vault_activation.
main.rs opts in only through PHILOTIC_SCOPED_VAULT_ENABLED=1, validates the inherited
named descriptor and root-controlled policy, and starts the endpoint using the
hotel graph directly. Disabled/absent configuration keeps it inactive. No general
IPC file was changed; Beacon's dirty worktree remains untouched.

LISTEN_PID must match the actual process. LISTEN_FDS is bounded 1..64;
LISTEN_FDNAMES must match its count and contain exactly one percival-scoped-vault.
Disabled startup rejects all LISTEN_* metadata and any inherited scoped socket FD,
even if unadvertised. This intentionally excludes unrelated socket activation in
disabled mode. Enabled startup rejects extra scoped aliases, advertised or hidden.
Kernel checks require AF_UNIX, SOCK_STREAM and SO_ACCEPTCONN. Both the inherited
original and adopted duplicate are close-on-exec. No bind/chown/chmod is performed.
The listener requires /run/percival-personal-vault-broker.sock, gateway owner,
mode0600 and a root-controlled parent; Linux SO_PEERCRED admits only the configured
nonroot gateway UID, distinct from the actual hotel UID.

Fixed policy: /etc/percival-personal-mcp/scoped-vault-policy.json. Only caller_uid,
hotel_uid, secret_ref; root-owned nonwritable ancestors/file, no symlinks, bounded
read and descriptor identity checks. No request-supplied role/ref/UID is accepted.
The actual encrypted vault resolver checks exact singleton role
percival-personal-recall and guest percival-personal-gateway on the same fetched
record it decrypts. The fixed permitted secret kind is percival-muninn-observe.
No live integration key has been created. Future key storage must be the encrypted
Philote vault. Synthetic credentials exist only in disposable acceptance fixtures.

## Shutdown

A separate broadcast requests listener cancellation at startup-test completion
and before normal guest drain. Cancellation closes owned handlers; signalling is
asynchronous, so this does not prove closure acknowledgement before drain.
Unexpected task completion/panic exits the process for supervisor replacement.
When enabled, every hotel_main return reaches runtime.shutdown_timeout(1 second), then main
returns and the process exits. Disabled startup retains the previous runtime-drop
behavior. That bound covers runtime teardown, not the existing
30-second guest drain or synchronous cleanup. A separate native-thread deadline is
now armed before drain/cleanup: 35 seconds on normal stop, 2 seconds at startup-test
completion. A native SIGTERM/SIGINT observer is installed before runtime/bootstrap,
so stop observation itself does not depend on async worker polling. The intended
35-second signal limit allows polling/OS scheduling latency. It forces process
exit even if the actual shared SQLite mutex prevents
pid cleanup from returning. Four stuck blocking resolvers retain
all four process-wide permits; only full process replacement restores capacity.

## Executed evidence

- Approved Ubuntu pkg-config 1.8.1-2build1 and libssl-dev 3.0.13-0ubuntu3.16 installed
  only in the existing Ubuntu24.04 aarch64 test container. Network disconnected
  before builds/tests. 2GB linker failure resolved by raising only this container
  to4GB. No host/production installation or setting changed.
- Full offline Linux aiua binary build passed; final SHA256
  a5e2a9128476bbb9bae2fd2478f6c97e244cc477300d4678bc4889ecc4b0beb3.
- Full Linux real-hotel acceptance passed: ephemeral real encrypted SQLite vault,
  UID1234 retrieval, actual materialized UID999 guest has no scoped descriptor,
  SIGTERM, retained supervisor FD fresh-process recovery and scoped startup-test
  completion. Invalid PID/aliases/disabled/unset/hidden FDs/smoke and root/wrong
  process UID reject before guest materialization. Supervisor is synthetic.
- Five targeted actual-aiua Linux lifecycle test entries passed, including real
  shared SQLite mutex blocking set_hotel_pid, real SIGTERM without async polling,
  and injected endpoint panic through the production supervision helper.
- Seventeen current Linux listener harness tests passed as UID1234.

- Full current-base aiua macOS check and binary build passed (existing warnings).
- 17 native Rust tests passed, including activation metadata validation.
- Actual aiua storage-lock subprocess regression passed: real in-memory SQLite
  connection mutex held while production set_hotel_pid cleanup blocks; independent
  shortened deadline forced process exit (2 targeted test entries passed).
- Linux actual optional activation/exec acceptance passed: selected original and
  adopted FDs absent in child; hidden extra alias rejected when enabled; disabled/
  unset mode rejects metadata and unadvertised scoped FDs before guest creation.
- Actual Linux listener/strict encrypted resolver and production FD adopter compiled
  and passed distinct-UID, root/wrong/hotel rejection, fixed socket/policy, framing,
  deadline/concurrency, stuck-worker replacement, bounded fixture exit/recovery.
- 75 synthetic Node gateway tests passed.
- Independent review found no additional authority bypass after shutdown fixes.

The isolated listener harness uses synthetic graph/key providers. The full-hotel
acceptance instead uses actual SQLite storage and production AES-GCM decryption
with public synthetic key/data. Its supervisor descriptor handoff crosses a real
exec boundary, but is not the installed production supervisor. Endpoint panic is
injected into the actual supervision helper, not a running installed hotel.
Root remains a trusted administrator. No production vault was accessed.

## Remaining gates

The full Linux source build and synthetic bootstrap/supervisor gates passed.
Installed hotel/vault/Muninn and actual production supervisor FD handoff remain
unverified. scoped-vault-bootstrap validates configuration/teardown only; CLI
success alone does not prove credential delivery or guest isolation. The external
admitted-UID and actual guest probe tests provide that isolated evidence.
Enabled IPC-only smoke mode now fails closed instead of skipping the endpoint.
The earlier split-specific patch and external Python broker are superseded.
No verified channel/session mapping for either authorized Claude conversation;
no message was sent. Private user_likesjx remains held.


## 2026-10-08 readiness correction — source/test only

Optional root-policy introspection_secret_ref is pinned under
secret://hotel/default/percival-issuer-introspection/<id>. The fixed operation
get_percival_introspection_credential has identical kernel-UID, bounded-worker
and exact singleton ACL enforcement. No request-supplied reference/role/identity.
Absent optional reference denies. Its43–128 base64url characters exclude mk_
keys and must come from >=32 random bytes. The gateway UID now receives TWO
fixed credentials, not arbitrary vault access or another client's credentials.

Gateway rejects raw issuer clientSecret/clientSecretEnv config and retrieves the
introspection credential afresh per request under cancellation/deadlines. The
isolated identity patch accepts a full Basic-header SHA256 verifier, not raw
issuer-secret env configuration. Provisioning still needs explicit approval:
generate random secret, encrypt directly in Philote, derive verifier in memory;
no plaintext staging/env/log output. Source does not create live credentials.

Deployment templates in deploy/ hide hotel data/general IPC in the gateway's
mount namespace. Actual installed paths, alternative sockets, backups/root-key
locations and namespace behavior must be checked before approval. Service
Sockets= associates the differently named socket; rollback removes that drop-in,
not an empty assignment (which does not clear the setting). Primary reference:
[systemd.service source](https://raw.githubusercontent.com/systemd/systemd/main/man/systemd.service.xml).

READINESS_DELTA.md records latest tests and architecture limits. The earlier
Linux source/test milestone did not establish production readiness.
