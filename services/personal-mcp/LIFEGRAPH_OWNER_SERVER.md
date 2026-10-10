# Supervised LifeGraph read owner

Source implementation follows PR654; no service, listener, journal, secret, credential, grant or production route is installed. Production namespace admission remains denied by the synthetic-only composition. The Muninn hotel rollout candidate `3144e429` lacks this owner and its writer guards; a separately reviewed newer binary is required.

`data_memorygraphrag::read_owner::ReadOwner` is an actual fixed-query Memgraph/Unix owner, not a caller-supplied transaction hook. It owns a private `LocalLaunchRegistry`, concrete supervisor-owned Child handles, role mapping, a daemon-wide cross-process mutex, bounded connection permits and a locked durable uncertainty journal. Attach/retire and peer requests share the authority fence. Kernel peer PID/UID, live child, stable identity and launch generation are revalidated. Canonical hotel and local service role come from trusted owner configuration, not wire JSON or personal OAuth grants. Unregistered/independently supervised/cross-host clients deny.

The listener is passed explicitly by its supervisor; private socket installation and access are reviewed deployment actions. The Node clients check private same-UID socket ownership/inode continuity and owner/session/sequence correlation. This is a local trusted installation boundary; it does not claim authenticated remote transport or safe enrollment of existing Docker containers by asserted PID. A deployment requiring cross-host issuer transport still needs a reviewed authenticated relay/topology, and remains denied.

Gateway `begin_snapshot` supports pre-admission reads. `begin_release` holds the daemon fence through actor/policy/revision checks and local HTTP transport handoff; nested reads use that same socket and Memgraph transaction. Issuer `begin_issuer_change` holds the same fence around its canonical Mongo mutation. The source-only issuer patch in jaredlikes-com enrolls actual revocation/admission/RBAC helpers. The graph mutation route is absent. Owned storage rejects its old process-local issuer/graph mutation wrappers so they cannot masquerade as cross-process enrollment.

All graph operations are four fixed parameterized queries. The owner constructs a private pool. The narrowly vendored neo4rs extension holds one physical checkout through session SNAPSHOT_ISOLATION setup, bounded SHOW STORAGE INFO verification, and BEGIN. There are no intermediate checkouts or retries; connection replacement before acquisition is followed by fresh setup and verification. Pool capacity alone is not relied on for physical session continuity. Memgraph3.10.1 rejects that metadata statement inside an explicit transaction. No other caller retains the pool and all query accesses share the daemon fence. The exact session-only isolation command is a deployment review item; no production setting was changed during this work. Streams bound rows and per-row/aggregate serialized bytes while accumulating results. Frame lengths are bounded before body allocation; commands, connections and lease admission deadlines are bounded. Neo4rs may decode a single server row before application bounds apply; this is not a driver-level packet memory cap.

Transaction ownership is retained until explicit awaited rollback acknowledgment. Driver futures are not timed out/dropped mid-operation. A stalled driver therefore retains its fence and requires supervised intervention: the five-second lease is an admission deadline, not guaranteed server-operation completion. Unexpected release/issuer disconnect, rollback uncertainty, task cancellation or panic poisons admission via an RAII Fence. The pre-existing private journal is marked dirty and fsynced before a lease and clean only after acknowledged completion. Exclusive file locking excludes a second daemon. A dirty marker refuses startup; restart cannot erase uncertainty. A dirty marker is health/fence state, never a second canonical privacy-policy owner.

HTTP `finish` is the local handoff linearization point, not remote receipt. Node coordinator releases the canonical policy reservation, then the daemon release lease, after response finish/close; premature channel loss fails closed. Gateway actor resolution remains the issuer owner's current-token/role/grant interface. No memory-only client gains LifeGraph scope.

## Synthetic verification

`cargo test --locked -p data-memorygraphrag --lib read_owner` verifies durable single-owner locking, replaced/unsafe journal denial, frame limits, unknown fields, actual supervisor-owned child serialization, unregistered peer denial, disconnect/restart refusal and aborted-task poison. An ignored helper is executed only as those actual registered children.

Build the native test binary with pinned Rust1.94.0/locked dependencies, then run only the approved disposable Memgraph integration:

```sh
services/personal-mcp/lifegraph-owner-tests/run-disposable-bolt.sh --approved-fixture-run /ABSOLUTE/COMPILED_OWNER_TEST_BINARY
```

The runner pins official3.10.1 by digest, publishes no ports, disables networking, seeds only the checked-in synthetic fixture, executes one exact owner integration test under uid101, and removes only its newly created container. The test proves a real fixed graph read and isolation metadata check while a second registered issuer child cannot acquire the release fence. No production address is accepted by the runner.

## Remaining bounded installation work

Parent first reviews `LIFEGRAPH_METADATA_PROBE.md` exact commands/projections. Establish actual runtime version/isolation/schema, canonical SQLite identity/schema/DELETE journal mode, complete provenance/catalog export authority, writer capability inventory and supervised issuer/gateway topology. Then specify exact binary/unit/socket/journal changes, rollback/reconciliation procedure, writer exclusion, independent client grants and namespace enablement for action-time approval. The journal must be preprovisioned in a private directory; this source never creates or clears a runtime journal automatically. Keep graph writes/dedup inactive. No unmanaged writer, PID assertion, cold restart or injectable hook is counted as production readiness.
