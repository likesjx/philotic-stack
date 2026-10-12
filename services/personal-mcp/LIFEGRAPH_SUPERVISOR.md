# Explicit LifeGraph read supervisor

This Linux executable constructs the existing `ReadOwner`, serves its private
Unix protocol and retains/enrolls actual directly spawned `Child` handles. It
adds no installation, identity resolver, OAuth scope, client grant, graph write
route or personal namespace. `lifegraph-read-supervisor --config /ABSOLUTE/PATH`
requires an operator-supplied manifest; there are no production defaults.

Build the Linux artifact with `cargo build --locked --release -p
data-memorygraphrag --bin lifegraph-read-supervisor`. Record the resulting
artifact hash and approved installation path before proposing an installation.
The source PR does not add this binary to a live unit or release deployment.

## Version 1 manifest

JSON rejects unknown fields. The existing manifest must be at most 64KiB,
owned by the runtime UID with mode 0600 in a same-UID private directory (0700).
The version is exactly `1`. All paths are absolute; parent traversal is rejected.

| Field | Required value and evidence |
|---|---|
| `hotel`, `owner` | Explicit nonempty identities, at most 128 bytes. The same owner identity is supplied to enrolled clients. |
| `uid` | Exact effective runtime UID. This executable cannot change UID/GID or enroll a different UID. |
| `socket` | Absent path ending `.sock`, at most 100 bytes, in a preexisting same-UID private directory. Binding never removes an existing socket. The supervisor sets mode 0600 and removes only its own inode after shutdown. |
| `journal` | Preexisting same-UID mode 0600 file in a private directory containing exactly `clean\n`. The owner locks it exclusively. Dirty/unknown markers require reconciliation; neither this executable nor restart clears them. |
| `graph_connection_file` | Distinct preexisting private file, at most 8KiB, with exactly JSON keys `uri`, `user`, `password`. Actual values come from the existing approved credential/configuration channel, never from repository examples or chat. Loading does not provision credentials. |
| `workers` | Explicit gateway and issuer entries; 1–16 entries and at most 16 direct processes total. No automatic replacement or worker restart. |

Each worker entry has exactly these keys:

| Field | Contract |
|---|---|
| `guest`, `agent`, `roles` | Unique guest/agent per entry, bounded server-loaded principal and role names. Guest excludes `#`; the actual enrollment name adds `#INDEX`. Multiple replicas in `count` share the explicitly configured agent principal. These are launch identities, not OAuth subject/creator mappings. |
| `role` | Exactly `gateway` or `issuer`; controls permitted owner protocol operations. |
| `count` | Integer 1–16, with the global total capped at 16. Every required worker must remain live. |
| `executable` | Object with exactly `path`, `sha256`; lowercase SHA-256 of an ELF executable. File must be root/runtime-owned, not group/other writable; runtime-owned artifacts must have no write bits. Setuid/setgid bits are rejected and kernel `NO_NEW_PRIVS` is enforced before exec. The supervisor executes the verified open inode through `/proc/self/fd`, closing the path replacement race. |
| `artifacts` | At most 64 `{path,sha256}` entries for scripts/configuration used by arguments. Verified before any launch. These paths are not kept pinned for later interpreter reads: the reviewed deployment must keep the complete code/dependency tree immutable for the worker lifetime. Executable hashing alone does not attest Node scripts, modules, shared libraries or runtime behavior. |
| `directory` | Existing root/runtime-owned working directory without group/other write permissions. |
| `arguments` | Explicit argv, at most 64 arguments, each at most 4096 bytes. No implicit shell is introduced. Launchers must remain the actual connecting process: daemonization, Node cluster forks and subprocess clients are unsupported. |
| `environment` | Explicit bounded key/value map; parent environment is cleared. Values must be operator-reviewed and non-secret; use approved credential-file references for service secrets. Coordination/supervisor keys cannot be overridden. |

The supervisor injects `PERSONAL_OAUTH_LIFEGRAPH_COORDINATION=required`, the
explicit owner socket/ID, and `LIFEGRAPH_SUPERVISOR_WORKER_INDEX`. No worker gets
an owner response until every worker is spawned and enrolled. Required-mode
issuer code must guard every authority mutation before its first persistence
operation. An arbitrary approved executable is not proof that this contract is
implemented. PR20 supplies these guards for its covered OAuth mutations only.

## Lifecycle and denial

All artifact/configuration checks precede launch. Partial launch failure kills
and reaps launched children and removes only this generation's socket. Kernel
parent-death signals kill direct children if the supervisor crashes. Descendant
processes are not enrolled or supervised: deployments must prohibit worker
daemonization and separately provide reviewed service cgroup cleanup.

Any required child exit fails the whole generation. SIGTERM/SIGINT stops new
admission, allows existing bounded five-second leases up to six seconds to
acknowledge completion, then aborts/joins remaining connection tasks before
killing/reaping workers. An aborted fence leaves durable dirty denial. Clean
shutdown permits clean restart; abrupt crashes may leave a stale socket, which
is never automatically unlinked. Operator recovery must establish that the old
owner/workers have stopped and remote work is reconciled before changing either
socket or journal. No restart loop, recovery override or automatic cleaning exists.

The supervised process must not run concurrent owner listeners or mutate its
launch configuration. Child stdout/stderr are suppressed to avoid accidental
credential/personal-content logging; the binary emits only a generic failure.
Deployment observability and service supervision require a separately reviewed
installation plan, not an inferred installation from this source patch.

## Installation inputs still required

Supply non-secret metadata: actual issuer/gateway hosts and process topology;
UID/GID/isolation requirements; exact immutable release paths, executable and
dependency manifests/digests, argv, working directories and replica counts;
owner identities; proposed private socket/journal/manifest paths and modes;
existing approved graph credential-file reference; service supervisor/cgroup
policy, shutdown budget and reconciliation operator. Consume the Muninn owner's
writer inventory rather than running a duplicate discovery workflow.

Existing independent or cross-host services cannot enroll by claiming PID/UID.
This entrypoint supports a co-located same-UID launch contract; it must not be
used to silently weaken the production gateway's existing UID isolation. A
cross-UID/cross-host relay remains separate design/review work if metadata
requires it. Installation, path/permission/topology changes, required-mode
activation, writer exclusions, persistent credentials/grants and recovery each
need exact action-time approval.

Identity bindings (`identityAuthority.resolve`, `issuerAuthority.resolveCurrent`),
canonical policy inode/schema/journal evidence and approved catalog digest are
still required for gateway composition. The current adapter permits only
`synthetic-read-only-v1`/synthetic namespaces; PR20 still accepts only
`memory:recall`. This supervisor does not activate personal graph access.

## Synthetic acceptance

`cargo test --locked -p data-memorygraphrag --test read_supervisor` invokes the
actual executable and real owned ELF children, covering malformed/digest/override
denial before launch, private paths, serialization, clean restart, required
worker loss, dirty restart, second-owner exclusion, existing socket preservation,
unenrolled peers, partial spawn failure and interrupted shutdown. The configured
synthetic graph endpoint is deliberately unreachable; these fence tests issue no
database queries. Existing owner acceptance separately covers disposable Memgraph.

The [composed acceptance fixture](lifegraph-owner-tests/COMPOSED_ACCEPTANCE.md)
combines PR20's actual Mongo helpers, HTTP response handoff, canonical SQLite
policy reservation and disposable Memgraph under this executable, including
both race directions and client/revocation isolation. It uses separately labeled
synthetic LifeGraph capabilities; PR20's actual memory tokens remain memory-only.
Passing this fixture does not attest production LifeGraph scope issuance,
installation, actor mappings or complete authority-writer coverage.
