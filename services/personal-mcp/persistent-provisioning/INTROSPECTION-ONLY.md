# Introspection-only transaction (local candidate)

This extends `provision.py` with `introspection-plan`, `introspection-apply` and
`introspection-reconcile`. It never calls the Muninn admin API, issues a native
key, adopts policy, enables OAuth or calls a production service on import/plan.
Historical plan/apply behavior remains unchanged. No caller chooses or restores
an old reference; the existing encrypted store always allocates a fresh UUID.

## Scope

Manifest is exactly version2, scope `percival_gateway_persistent_recall_v1`, role
`percival-personal-recall`, guest `percival-personal-gateway`, plus the actual
helper SHA256. No synthetic hash is an acceptable installed artifact attestation.
Use isolated Python and the existing protected owner terminal/private channel.
Apply requires fixed installed paths/provenance, protected root process, TTY and
exact `CREATE_INTROSPECTION <manifest digest>` consent before private input.
Do not retry the previously denied agent-private terminal/FD path. This code does
not discover keys or assert the installed handoff is currently available.

`Broker.store_exact_acl` reuses existing encryption, readback and key protection.
Its new introspection-only handshake announces a freshly allocated reference;
the parent persists that reference and verifier to the existing private Journal
before sending a commit acknowledgment. The helper cannot write before ACK.
No plaintext credential enters the journal/stdout. The journal retains the hash
verifier, allocated reference, manifest and pending/done state; it must stay private.
Raw credentials remain process-local; no memory-zeroization guarantee is made.

## Lost replies and hard crashes

Before durable allocation/ACK: no record can be written. After allocation, a
SIGKILL or lost final reply leaves a known reference and a pending operation.
Explicit reconciliation requires prior helper termination/quiescence attestation;
it checks only that allocated introspection record under exact scope/ACL and
compares the derived verifier internally. It returns stored/absent/mismatch only.
A stored observation is labeled `observed_stored`, not a claimed reply ACK.
An absent or conflicting result does not authorize a second creation. Repeated
apply on an existing journal is refused. There is no automatic retry or broad scan.

## Policy/verifier adoption

The `transition` function orders the reviewed owner-adapter effects under the same
private journal: close gateway, publish policy reference, publish verifier,
restart the *same* hotel binary to load policy, adopt issuer verifier, verify pair.
`introspection_owner.Owner` now implements the fixed host operations: private
baseline capture, inode/byte CAS with directory fsync, narrow new-reference policy
replacement preserving unrelated bytes, paired-verifier publication, bounded
same-binary hotel restart and pinned issuer Compose adoption/rollback. The new
`introspection-adopt` / `introspection-recover` CLI modes consume a separate exact
adoption manifest and typed digest confirmation. The manifest binds the actual
policy/verifier hashes, selected units/source files, image, Compose chain/env hash
and fresh observation. It enforces issuer OAuth=false and gateway stopped before
publication. This is a repair boundary, **not evidence that DB admissions are
disabled**; authenticated admission preflight remains separately required before
OAuth activation. Missing socket-unit properties are explicit nulls, not invented
values. Root-owned sticky /tmp alone is an ancestor exception for allowed existing
Compose overrides; lower ancestry/files remain ownership/writeability guarded.

Private override allocation records its inode durably before writing. Recovery
removes only the recorded owned override, including a known interrupted prefix;
unknown replacements stop. A pinned image-only rollback override remains until
its loaded Compose reference can safely be removed. The original source files and
unrelated bytes are never overwritten. Command output is capped while streaming,
and owned process groups are reaped even if their leader exited. The getter uses
protected fork/no exec after UID1003 transition, exact listening socket-inode to
current hotel association and same-netns before/after. It returns only the derived
verifier internally. No secret value enters stdout/journal.

The owner CLI requires the fixed trusted installed paths and protected owner
terminal. Actual installed source/helper/channel and fresh baseline inputs are
still missing; this is not an executable production approval. TTY consent in the
Linux qualification is explicitly simulated with public dummy input, not proof of
human consent or a bypass of the denied agent-private terminal.

Known public source loads Policy once before serving, then resolves secrets per
request. Changing a file is not a hot reload. Model tests exercise that cache and
an explicit same-binary restart; they do not establish installed B7 source identity,
Linux supervisor behavior, hook safety, guest health or an approved live restart.

Failure/termination leaves pending state. No automatic adoption retry or rollback.
After explicit prior-operation-finished evidence, recovery restores the captured
policy/verifier and loaded selection through the reviewed owner effects and checks
closed access. It restores the previous baseline, which may be nonfunctional;
it does not claim that the missing old record now works. Unknown recovery effects
stop and retain evidence. Never blindly delete an unproven owned record, revert an
unrelated database, renew/revoke a native key or replace/restart the production
hotel as a side effect of testing.

## Local qualification

Run the Python provisioning test directory and offline persistent-helper Rust
suite. Tests use synthetic data only: real subprocess allocation/ACK transport;
actual SIGKILL before/after allocation, after store-before-reply, and during policy
adoption; journal persistence, no duplicate create, scope/ACL/crypto readback,
immutable policy model, explicit recovery, actual CLI plan and foreign-install
apply refusal. Existing Linux-specific protected handoff integration remains a
separate prerequisite; no agent-private FD execution is qualified by these tests.

## One action-time package, not authorization

Before a live command, bind:

1. This reviewed local source and rebuilt helper hashes, installed helper/channel
   identity and exact existing encryption-authority source/recipient. No secrets
   are presumed missing, searched for, or requested in chat.
2. Fresh fixed policy and verifier baselines, original native reference preserved,
   target new-reference lifecycle, issuer public configuration/Compose image and
   file chain, and required gateway/hotel/socket dependency/drop-in consequences.
3. Qualified existing owner-adapter wiring and isolated full adoption/recovery
   rehearsal, including guest health and command deadlines. Source/model tests
   do not substitute for this evidence. Shared hotel interruption is explicit.
4. New persistent introspection record scope/lifetime, verifier rotation/policy
   adoption, private capture retention and uncertainty/rollback approval. No native
   key creation; recorded native expiry October26 18:42UTC remains independent.
5. Native operational association/expiry, exact restricted admission writer setup/
   lifetime, disabled0 seed, separate0→1 enable and forward1→2 disable, promotion
   persistence, owner Claude consent and agreed non-sensitive recall/denial/
   revocation/recovery acceptance.

The current production hotel, accepted bridge, policy, credentials and admissions
were not inspected or changed by this local candidate. ProductionReady is false.

## Coherent isolated Linux qualification and pinning

`.github/workflows/muninn-introspection-only.yml` is push-only for this feature
branch, contents-read, no production secrets/SSH/credentials/deployment. It runs
Python fixtures and locked Rust1.94.1 Linux tests/build; reuses the existing
same-UID hostile-sibling private-key handoff fixture; then executes
`linux_introspection_acceptance.py --disposable-public-fixture` on an empty
GitHub-hosted Linux runner with root. The latter installs only dummy state and
fixture principals, uses the actual helper/SQLite/private handoff/entrypoint,
actual systemd socket + Python hotel + gateway, and actual Docker Compose with a
pinned public nginx fixture issuer (network_mode none). It executes real Owner
capture/CAS/restart/verifier pairing/recovery and an unloaded drop-in refusal.
It stops/removes containers, units, fixture accounts and fake state afterward.

Production B7/image pins are replaced only inside the isolated fixture process
with the actual synthetic interpreter/image identities; production source and CLI
have no fixture switch. Output explicitly disclaims installed B7/production issuer
qualification. This tests the real adapter mechanics and assembled command paths,
not live guest safety or deployment inputs. A successful run is still required;
Mac syntax/source/model tests must not be described as Linux acceptance.

The workflow builds the Linux helper and records SHA256 plus exact SOURCE commit,
then uploads only the helper and pin files for one day. No DB, key, policy, verifier
or private fixture journal is uploaded. Published source commit + successful run +
artifact checksum are required before proposing its installation. The workflow and
fixture are part of the same local patch; no later workflow publication is hidden.

## Persistent issuer baseline input

Owner manifest schema2 reuses the canonical issuer adoption contract: project
`jaredlikes-com`, directory `/home/deploy/jaredlikes-com`, exact content-addressed
`deployment-baselines/616c8b0a4dae7182177b393750193b19ba9648bf5d6029f7593c937f41da93a4/compose.candidate.json`,
then only persistent `issuer-configurations/<run>-<attempt>/production-digest-promotion.override.yml`
files. The baseline bytes must match that SHA256; a JSON suffix grants no trust.
Overrides retain the closed dependency filter. A JSON override may reference
only the fixed existing verifier env file, already captured/guarded by this
adapter. No new indirect file is admitted. The historical YAML filter is a dependency
filter, not a semantic service/privilege whitelist: later application still needs
explicit approval of the captured baseline and exact hashes. Schema1 retains its
historical path and recovery rules; schemas cannot mix chains.

The paired private issuer checkout contains a capture-only payload builder that
selects just the read/guard methods from this source. Capture requires separate
approval because it copies potentially secret-bearing configuration to private
server storage. It does not need the helper, encryption input, a getter or service
mutation. New local persistent-path tests exercise synthetic bytes and substituted
fixture hashes; they do not verify the actual production baseline or supersede
the Linux qualification of the earlier source revision.
