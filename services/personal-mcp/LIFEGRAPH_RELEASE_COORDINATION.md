# LifeGraph release coordination: bounded synthetic implementation

Based on merged develop `37ba55efb6b3730f977795f60915c64922957bcb` (PR650
and PR652). Production composition remains disabled and production namespaces
remain rejected. No issuer, broker, server, infrastructure or rollout files change.

## Implemented boundary

`lifegraph-release.mjs` implements a process-local authority gate, bounded to
32 waiting operations by default (maximum 64) and a five-second lease maximum.
It resolves current issuer identity/grant inside the gate, obtains an actual
policy-owner reservation, checks the current graph revision and synchronous
canonical SQLite validator, then retains both the gate and policy reservation
through HTTP response finish/close. A request-bound single-use permit binds the
pre-serialized payload digest; the gateway verifies that exact text before sending.
Timeout, disconnect and owner close destroy the response and release reservations.
Failure to release a reservation closes the coordinator before queued work can
enter. An uncooperative owner fails closed; supervisor recovery is required.

`participateIssuerChange(context, mutation)` and `participateGraphChange(context,
mutation)` wrap an authority owner's existing transaction. They wait for admitted
responses to finish before performing revocation or mutation. They do not create
grants or authorize writes. All grant/role/identity and relevant graph writers,
including administrative paths, must participate in this same owner. A remote
issuer with introspection alone cannot satisfy this protocol. Acquire the gate
before the canonical policy reservation, then graph work; no writer may acquire
these in reverse order. Do not call another coordinator operation recursively
inside a participating mutation. Production drain acknowledgement must occur only
after the participating transaction has completed. Previously handed-off bytes
cannot be retracted, and this does not claim remote-model inference has stopped.

The initial implementation serializes admitted reads and participating changes
globally. This trades throughput for a small, verifiable bounded boundary. Client
grants remain separate; revoking dot does not change Claude's authority. Scaling
to per-client/namespace fences is a later reviewed change, not implicit sharding.

Policy reservations belong to the canonical store owner. Reuse Rust
`PolicyStore::pin_commit`/`PolicyCommitLease`: `BEGIN IMMEDIATE` on the same
canonical SQLite file blocks other policy writers. The gateway's read-only
connection cannot implement this reservation. The test owner uses the same real
SQLite reservation against a temporary fixture only; no production database is
opened with write permissions. No policy mutation is introduced by this code.

Response hooks are optional for compatibility with the earlier synthetic
injected-barrier tests. Lifetime enforcement described above applies only to the
new coordinator. An arbitrary legacy boolean barrier is not production admission.

## Existing-schema reader

`lifegraph-memgraph.mjs` uses fixed parameterized read queries and an immutable,
server-owned catalog. Each namespace has an exact record allowlist, a closed
ontology mapping, full summary/label/source content digest and canonical-root
bindings. No graph `namespace` property or global revision node is invented.
Ideas map to existing `GrowthHypothesis` records with `idea:` IDs; Thing maps to
approved Asset/CreativeWork labels. Canonical variants require a unique existing
`LifeRootBinding` verified key or explicitly approved `LifeRootAlias`. Ordinary
`SUPERSEDES` edges and normalized-summary equality are not canonical authority.

The reader verifies complete graph source-policy manifests against the approved
catalog. Storage additionally requires those manifests to equal the immutable
canonical SQLite ancestry. Privacy remains enforced by canonical policies, not
labels or graph-provided ACLs. Missing records, duplicate/ambiguous roots, changed
content/provenance, unknown namespace or unapproved ontology deny the entire read.
The adapter projects only authorized canonical roots and induced authorized edges.

The transport must run a bounded real snapshot read transaction and rollback on
failure. The checked-in Rust line transport is explicitly a disposable synthetic
acceptance fixture, not a production transport or listener. It uses fixed loopback
inside the disposable container namespace and repository-pinned dependencies.
Some driver row materialization precedes byte checks; a production transport needs
its own wire/allocation limits, cancellation and authenticated owner IPC.

## Actual Memgraph compatibility finding

Memgraph 3.10.1 rejects the older unexported capture prototype's
`SHOW STORAGE INFO ON CURRENT DATABASE`. `SHOW STORAGE INFO` is accepted and
reports `global_isolation_level`, `session_isolation_level` and `storage_mode`.
The session level is initially empty even when global isolation is snapshot.
The acceptance fixture uses a one-connection driver pool, executes
`SET SESSION TRANSACTION ISOLATION LEVEL SNAPSHOT ISOLATION`, and checks the
explicit session level and transactional storage mode on that connection.
The unrelated capture prototype is unchanged; its owner should review this
compatibility issue before activating capture.

## Disposable verification and remaining owner decisions

Official linux/amd64 images used only in the cloud test environment:

- Memgraph 3.10.1: `memgraph/memgraph@sha256:bd01a159023283b56b807943ed28225c8f662920b64317f26da7d9f0f5b19de4`
- Rust 1.94.0 slim-bookworm: `rust@sha256:8e0f5926ce10ec99e12703b315c0043e4709d6bdc740dc1d28753e5fb87c766f`
- Driver: repository-existing `neo4rs =0.9.0-rc.9`, with a checked-in fixture lockfile.

The Memgraph container has `--network none`, no published host ports, dropped
capabilities, `no-new-privileges`, one CPU, 1 GiB RAM, bounded PIDs and disposable
tmpfs data/log storage. The Rust build has two CPUs, 2 GiB RAM and bounded PIDs;
network is used only to fetch public locked dependencies. The existing public
cloud CA bundle is copied into that disposable build container so TLS remains
verified. No production routes, mounts, grants, tokens or host security changes.

`lifegraph-memgraph.acceptance.mjs` verifies the exact image, absent port bindings
and isolated network before running real Bolt, temporary SQLite and loopback HTTP
acceptance. It requires a freshly seeded disposable container and fixture binary;
it intentionally mutates only its synthetic graph to test races. `npm test` runs
the default credential-free service tests; real Bolt acceptance is opt-in.

Recorded validation: Node 24.19.0 service suite passed 114/114; `npm run check`,
explicit new-module syntax checks, shell syntax and whitespace checks passed.
The pinned Rust 1.94.0/neo4rs fixture compiled, and real Bolt acceptance passed
its integrated test: all six ontology kinds, canonical root collapse, ambiguous
approved alias denial, private Person/hidden edge suppression, client isolation,
actual separate-connection SQLite writer exclusion, queued issuer revocation and
graph mutation until response finish, independent Claude continuation, changed
content rejection, and an MVCC stale-read negative control.

Reproduce only in an explicitly approved disposable environment:
`bash services/personal-mcp/lifegraph-memgraph-tests/run-synthetic.sh --approved-fixture-run`.
The recipe refuses pre-existing fixture containers and removes only containers
it created on exit. Registry images remain a test-environment cache; no runtime
service is installed. The prior base's postmerge workflows all completed green:
PR Check 38075834133, Build Linux 38075834095 and Percival acceptance 38075834004.

Still required for production: the deployed canonical PolicyStore owner/path and
complete record-to-resource/content catalog; issuer owner implementation enrolling
every existing grant/role/revocation transaction in this release protocol; graph
owner enrolling every relevant writer; supervised authenticated IPC between those
owners and a hardened production Bolt transport. A static catalog here is a
bounded synthetic binding, not an inferred production backfill. Unknown legacy
records stay denied. Exact callbacks are a later client-registration input.

Independent exact-head source review and CI precede merge. Action-time approval
still covers exact persistent grants, credentials, policy/role bindings and
server/security rollout. Parent owns architecture/task updates to avoid overlap;
no real graph/memory bootstrap, telemetry or continuity writes were performed.
