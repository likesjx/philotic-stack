# Native memory OAuth implementation checkpoint

Status: local contract implementation, **activation disabled**, 2026-10-29.
Base: philotic-stack 7399da86; paired issuer base jaredlikes-com d1afd9f794bfb38a412618ff80b98967abddc01c.

## Current checkpoint (supersedes earlier evidence below)

The user approved hotel-owned supervision and enforcement. Aiua's existing
materializer now has an opt-in native authority runtime; the existing MCP
membrane is extended, not replaced. The runtime owns authenticated concrete child
generations and a shared durable uncertainty fence for issuer mutation, native
commit, and HTTP result handoff. The runner reserves through the private hotel
socket and revalidates immediately before Memgraph COMMIT. The membrane obtains
release authority after signed response verification and retains it until HTTP
handoff; delayed first polling revalidates and has a conservative deadline.
No OAuth bearer is put in routed tasks. Those carry the signed envelope and an
opaque request-bound permit handle. Reusable credentials stay local.

This implementation is not yet runtime-qualified or published. It requires a
same-hotel membrane, runner and issuer authority worker. General cross-hotel OAuth
fencing is not implemented. The new protected runtime is Linux-only; the existing
Mac interactive bearer test remains unchanged. The issuer activation gate still
returns 503 until composition qualification is complete.

Current evidence: issuer typecheck passed; policy/coordination/authority suite
63 passed and 2 skipped after locked dependency installation. Linux cargo check
passed for aiua, membrane-mcp, native-memory and data-memorygraphrag. Six hotel
authority tests passed on Linux, including two actual owned child processes using
the private control protocol (mock issuer/storage). Two durable-journal tests
passed after closing the original owner before testing recovery. Eager child
enrollment/retirement/replacement passed on macOS, including denial before adopt.
Membrane suite: 35 passed, 1 ignored; native-memory: 23 passed, 3 database cases
ignored. Native-memory all-target Clippy with warnings denied passed. Two HTTP
release-stream tests verify expiry/denial and drop semantics. The Linux-only
full HTTP/OAuth/hotel/Memgraph fixture compiles but is not yet executed; sealed
Node bundle execution and complete composition remain CI qualification gates.
Earlier evidence below is historical, not proof for later changes.

The independent reviewer found delayed HTTP polling could release cached bytes
after lease expiry; immediate validation and an absolute handoff bound were added.
Executable and issuer bundle admission is being strengthened with Linux sealed
snapshots and Node preserve-symlinks-main. Actual Node FD loading, process startup,
crash/replacement, and complete hotel-managed OAuth/Memgraph composition remain
qualification work. No production credentials, grants, services or routes changed.

## Implemented

Paired issuer policy defines a distinct `nativeMemoryResource`, two scopes
`native-memory:recall` and `native-memory:remember`, and a separately provisioned
resource admission keyed by `native-memory:access`. That key is an internal grant
slot, never a requestable OAuth scope. The admission's `nativeScopes` limits
requested permissions; refresh cannot add scopes. Existing memory and LifeGraph
grants cannot authorize the new resource. Existing PKCE, consent, confidential
introspection, rotation and version checks are reused. Native management changes
increment the existing admission version under the existing issuer coordinator.

Rust `native_memory::oauth` validates exact issuer, resource, client, subject,
global/resource version, local operation mapping, native scopes and token times.
Owner and namespace come from local configuration, not the OAuth caller. It does
not fetch introspection, authenticate an HTTP request, identify a token family,
sign a native request, or install a runtime route. Its public deserialization
types are data contracts, not proof of authority.

The issuer runtime explicitly rejects attempted native-resource activation with
503. No native env setting, real admission, credential, listener or service changed.

## Earlier architecture assessment (decision now made above)

Full-target milestone retained: remote hotel enforcement over the existing router.
The first release co-locates MCP, runner and private Memgraph on one VPS hotel.
Local permit handles are intentionally not portable authority: mismatched
destination/guest bindings deny. A future versioned remote enforcement protocol
must let the destination hotel verify scoped delegation, origin/destination
incarnations and request binding, fence commit and result release against
revocation/cancellation, and preserve uncertain/replayed outcomes across restart.
Reusable issuer/database/vault credentials must remain at their owning hotel.
Qualification must include partition, delayed/reordered delivery, peer restart,
expired delegation, replacement guests, duplicate attempts, and revocation racing
commit/release. The protocol is not implemented or implicitly approved for live
use by this local socket adapter. No distributed fence algorithm is being added
to this first slice; the parent supplied a separate protocol assessment.

The remote assessment proposes `Admit → Prepare → AuthorizeCommit →
CommitReceipt → AuthorizeRelease`, plus `Cancel`, `Revoke` and
`GetOperationStatus`. A versioned signed envelope must bind coordinator/issuer,
destination/resource/runner generation, owner/principal/client/namespace,
authority and policy revisions, operation/attempt/idempotency, exact action and
canonical arguments, lifetime/replay ID and result recipient. Mesh encryption
and authentication require executable verification; signatures alone are not
confidentiality. Remote authority unavailability must deny new commit/release.

Local ordering boundary: issuer mutations using the enrolled authority worker
hold the same journal lock as native commit and release. A commit reserved before
revocation can finish; revocation waits and cannot promise retroactive rollback.
Direct database/admin writes, uncoordinated issuer instances and upstream
identity-provider changes are not serialized by this fence. Fresh introspection
can observe them but does not make them atomic with COMMIT.
The local runner holds its reservation from validation through database commit
acknowledgement; generation retirement uses that same fence. A crash/unknown
acknowledgement leaves a durable dirty marker and prevents automatic recovery.
The database transaction persists event receipts and consumed nonces, while
volatile hotel permit handles do not survive restart and cannot be revived.
This is not a database-enforced generation token: backend-side fencing and
cross-hotel uncertain-operation reconciliation remain explicit remote protocol
qualification requirements. Recovery must reconcile by the existing operation
ID, never blindly reissue a write. Production approval must limit database writers
and issuer mutation paths to the qualified boundary.

The existing Linux `ReadOwner` serializes issuer mutation and gateway release via
a durable dirty/clean journal. Unknown completion poisons admission across
restart. It owns a private launch registry and authenticates concrete owned child
handles. Native membrane and runner guests instead belong to aiua. aiua has an
opt-in `ProtectedLocalGuest` adapter and a separate authenticated launch registry.
Neither path currently gives a queued native write a live coordinator permit.

**Recommended:** reuse the existing fence/journal and authenticated launch
registry under aiua's guest lifecycle, separating generic coordination from the
fixed LifeGraph read query interface. Enroll the issuer authority worker through
that same canonical launch owner. This expands the hotel implementation beyond
the qualified five-line B7 route backport; its previously qualified binary is
not sufficient. It also puts coordinator lifetime and authority-worker custody
under the hotel, so it must be an explicit architecture choice before rollout.

Alternative: have the existing read supervisor own the native guests and connect
them to hotel IPC. That retains its child authentication but splits guest lifecycle
ownership and needs independent lifecycle/recovery qualification. It is not an
already-qualified shortcut. Reject alternatives that accept claimed PIDs, trust
same-UID alone, duplicate the authority database, or add another gateway.

The live permit must bind token fingerprint, verified authority versions, request
digest, destination and guest incarnation. Receiver must acquire/validate it at
commit; membrane must validate it again at result release. A disconnect or unknown
commit must leave durable uncertainty and prevent an acknowledged revocation from
racing a continuing write. Successful revocation invalidates queued requests.
Checking introspection twice without a shared fence leaves a race and is insufficient.

Required deterministic composition tests: revoke before dequeue; revoke between
ingress and receiver; revoke while commit is reserved; cancel/lose permit during
commit; unknown commit then process restart; revoke after commit before release;
substitute token with same claims; delayed delivery to a replacement guest;
duplicate event with a new permitted attempt; signed response substitution.
Existing native replay/result tests are not evidence for these OAuth races.

## Earlier executed evidence

Issuer policy/handler tests: 50 passed, 2 existing external integration cases
skipped (52 total), using Node 22's built-in TypeScript transform via a temporary
loader. No npm install. Full issuer build/typecheck and Mongo/authority tests
have not run: this isolated worktree has no installed mongodb/zod/Next toolchain.
New management default/disabled-mode tests are written, not yet executed.

Rust: 3 new OAuth contract tests, 5 signed-response tests, 15 existing vertical
tests passed. One Unix IPC test initially failed at socket bind under the sandbox;
the same test passed through standard approved escalation. Three disposable
Memgraph cases remain ignored in this run. Offline Clippy all targets with
`-D warnings` passed. No real database or OAuth client used.

Independent review identified the unfenced native mutation default and an
overstatement about token identity. Guard now checks native command even without
resource env; comments identify the missing token fingerprint. Reviewer confirmed
both fixes and the unresolved lifecycle/permit boundary. No publication or runtime
qualification is claimed.

## Resource guardrail evidence and proposed deployment limits

Earlier isolated AMD64 MAGE 3.10.1 qualification on this ARM Mac verified WAL
fsync every transaction, recovery, 300-second snapshots, snapshot-on-exit and
capture/receipt/replay across SIGKILL. Dedicated stopped fixture:
`native-memory-fsync-qualification`; its dedicated volume remains preserved.
Actual Docker limits inspected: 2 CPUs, 2 GiB memory, 256 PIDs, json-file logs
10 MiB × 3. These are test-container limits, not demonstrated production sizing.

Proposed disk policy: 5 GiB data budget and 8 GiB host free-space floor, checked
before starting capture and every 60 seconds; stop new capture on threshold.
This is not a hard quota and has not been installed. Never delete observations,
receipts, nonces or WAL automatically. Docker stdout limits do not cap Memgraph's
internal file logs; internal logging/rotation needs explicit configuration and
qualification. Native guest systemd CPU/memory/task/log limits have not been
executed or qualified; systemd tooling is absent from existing local test images.

No production approval is requested for an incomplete bearer-only island. After
the architecture choice and qualification, one concrete approval package must
identify hotel artifact/restart, issuer worker custody, principal-owner mapping,
resource/namespace/operations/expiry, verifier pins and private credential files,
database privileges, HTTPS route, rollback and actual dot plugin setup.
