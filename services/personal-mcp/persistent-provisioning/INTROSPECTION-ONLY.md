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

The `transition` function orders existing owner-adapter effects under the same
private journal: close gateway, publish policy reference, publish verifier,
restart the *same* hotel binary to load policy, adopt issuer verifier, verify pair.
Owner guards must bind actual baseline/source/paths, same-binary provenance,
closed public access and guest/dependency readiness. Capture before effects and
record pending before every action. The adapter is supplied by the existing
reviewed lifecycle; this library is **not a production host/CAS/service adapter**.
It cannot be called executable production adoption without that integration and
fresh baseline/approval inputs. Existing `percival_memory_host`/journal roles own
policy CAS/same-binary service transitions; issuer adoption remains separately
bound to the exact existing issuer contract.

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
