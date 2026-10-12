# Canonical capture transaction slice — runtime off

The new isolated integration also provides `capture_local_authority.rs`, a
concrete bridge from kernel-verified local task authority to this transaction.
See `../ansible-mesh-core/LOCAL_AUTHORITY_INTEGRATION.md` for the tested child/UDS
path, v1 consumer API and exact uninstalled hotel/SDK boundaries.

`resolve_plan.rs`, `capture_commit.rs` and `capture_memgraph.rs` remain unexported.
The commit orchestrator is exercised end to end: authenticated synthetic source
policy → SQLite inbox enqueue/reopen/load → validated plan → durable fixture graph
transaction. The actual neo4rs/Memgraph adapter compiles through the shadow test;
no Memgraph instance was contacted. Its Cypher, metadata-result format, SHOW query
support inside an explicit transaction, isolation and constraint behavior require
validation against a disposable Memgraph instance before installation.

## Implemented writes and replay

The adapter uses one existing LifeGraph neo4rs transaction for authoritative
namespace/key resolution and all binding/root/extension/evidence/receipt writes.
No second production graph store is introduced. New roots use closed ontology
labels and a namespace/key-derived ID. Existing roots receive additional evidence
or a separate extension; their original claim is not overwritten. Every evidence
record and derivative carries the source policy manifest. Evidence IDs are part
of the persisted capture body/digest and must exactly match the submitted plan.
The server issuer must also verify those IDs refer to authentic source evidence.

The unique namespace/key binding is independent of root type. A stale absent plan
must abort and be explicitly re-resolved; there is no create-on-conflict fallback.
Event receipt identity binds complete payload digest, evidence IDs, source
manifest, server-authenticated actor, root anchor and ontology type. Exact replay
returns its previous result after current authorization; conflicting replay
rolls back. Ambiguous captures persist review receipts without creating roots.

## Revocation barrier

`PolicyStore::pin_commit` opens a separate connection to the same canonical policy
SQLite file and obtains BEGIN IMMEDIATE before current authorization. The owned
PolicyCommitLease contains that connection and policy snapshot; no JSON field or
boxed placeholder can issue it. The commit orchestrator requires this concrete
reservation, verifies its revision and retains it through graph commit/rollback.
All SQLite policy writers to that file, including other processes, serialize with
it or fail busy. Tests verify separate-connection owner revocation blocks while
pinned, succeeds on release, and denies subsequent handoff/replay. If revocation
wins before acquisition, no graph writes occur. If commit wins, revocation becomes
effective after that commit. Error paths release both graph and policy guards.

This is a policy/graph ordering barrier, not a distributed atomic transaction.
Crash after graph commit before inbox acknowledgement is handled by receipt
redelivery; inbox lease/ack/recovery remains an integration requirement. Acquiring
the SQLite reservation is blocking and must run on server blocking workers, not
an async executor needed by another lease holder. Production session expiry,
root write authority and authenticated evidence resolution remain issuer duties.
Runtime is off until that issuer is installed.

## Schema and migration seam

Require unique `id` on the selected ontology label, and unique `key` on
LifeRootBinding, LifeCaptureReceipt, LifeCaptureEvidence and LifeCaptureExtension;
alias resolution also requires LifeRootAlias.key uniqueness. The adapter checks
constraint metadata in the transaction. A single existing LifeCaptureSchema node
with key `resolve-before-create-v1`, version 1 and backfill_complete true is
required. A graph owner must establish that the catalog covers existing roots;
an empty new catalog must not enable duplicate roots. No schema installation or
backfill is included or executed.

The adapter requires transactional storage, current-database SNAPSHOT_ISOLATION
and an explicit snapshot session setting; missing/unknown metadata denies. The
owner must exclude NEXT isolation overrides and concurrent schema/configuration
changes during captures, or provide a stronger versioned schema guard. It does
not silently substitute older metadata queries. See official [constraints](https://memgraph.com/docs/fundamentals/constraints),
[transactions](https://memgraph.com/docs/fundamentals/transactions) and
[server stats](https://memgraph.com/docs/database-management/server-stats).

## Evidence and blockers

`tests/capture_atomic.rs` uses a serialized, file-backed reference graph fixture,
real SQLite policy storage, and the actual commit orchestrator. It checks atomic
rollback after all writes, reopen/idempotent replay, replay conflict, root and
extension evidence, concurrent contenders followed by explicit replan, durable
review, missing prerequisites, and both revocation orderings. This fixture's
serialization/durability does not prove Memgraph conflict/isolation behavior.
The separate-connection barrier test is actual SQLite evidence, not a mocked lock.

No Memgraph executable is installed. Docker inventory failed with permission
denied on `/Users/jaredlikes/.docker/run/docker.sock`; no disposable graph endpoint
was supplied. No dependencies were added or installed, no live graph records were
accessed, and no service was started/restarted.

IPC ownership blocks runtime installation, not this source/test slice.
`philotic-client::IpcResponse::InboundTask` has only source_node/task_id/task_json.
The IPC owner must add a server-resolved authenticated authority handle binding
the full task payload, source/evidence manifest, current policy revision and
stable identity/roles; preserve it through parked delivery and SDK decoding; and
install guarded provider/capture adapters without a permissive legacy path.
Client JSON and client voice preferences cannot supply these rights. The active
IPC, voice, Beacon and architecture worktrees were not edited.
