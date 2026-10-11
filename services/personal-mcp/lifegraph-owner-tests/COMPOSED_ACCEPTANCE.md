# Synthetic composed acceptance

This fixture depends on PR658's exact supervisor source head
`165003b7b9811a75c5e358ad1deb2f91d2d68e5b` and the separately reviewed PR20
issuer source head `bc97105557c30a17b9f966c1e1d0453c66c6f3ef`. It never installs
services or reads configured databases. Build the supervisor from that source
with `cargo build --locked -p data-memorygraphrag --bin lifegraph-read-supervisor`.
Use a clean exact issuer checkout with its locked npm dependencies installed.

```sh
python3 services/personal-mcp/lifegraph-owner-tests/run-composed.py \
  --approved-fixture-run --supervisor /ABSOLUTE/BUILT/lifegraph-read-supervisor \
  --issuer-source /ABSOLUTE/EXACT/ISSUER/CHECKOUT
```

Linux, Docker, Node 24+ and the issuer checkout's esbuild are required. The runner
verifies the issuer commit and clean tracked source, hashes the supervised Node
ELF and bundled worker, and creates uniquely named pinned disposable MongoDB and
Memgraph containers. An internal Docker network has no published host ports;
database data/logs live in tmpfs. The disposable runner executes the real
supervisor with real issuer/gateway children, private manifests, the exact
canonical SQLite v2 schema/inode, catalog digests, owner transport and actual
HTTP recall. Child environments are explicit; inherited production configuration
is never loaded. All created containers/network and temporary files are removed.

Actual PR20 authorize/consent/token/introspection handlers issue and check distinct
dot/Claude **memory-only** tokens. Those tokens fail LifeGraph dispatch. Separately
labeled test-only `synthetic-life-*` capabilities exercise LifeGraph mechanics:
their fixture issuer checks the actual memory family's current Mongo validity
and versions before presenting synthetic LifeGraph claims. They are not evidence
of an issued LifeGraph OAuth grant, and none of that fixture projection is
imported by production code.

Assertions cover creator access, independent role access and revocation, private
records/ancestors/incident edges, external inference denial, canonical-root
projection, dot/Claude isolation, revocation before final release and both fence
race directions. A competing SQLite writer is excluded while the actual HTTP
response is held; the actual Mongo helper's first write cannot run until handoff
finishes. Global admission, provider logout and token revocation use actual
issuer helpers. An unknown committed Mongo result leaves durable dirty denial
through supervisor restart. Negative results assert precise status/body or
successful filtered packets rather than treating any backend failure as denial.

## Canonical binding interface and remaining gates

`createLifeGraphIdentityAuthority` supplies both initial `resolve` and fenced
`resolveCurrent`. Its `resolveBinding` provider must return exactly the requested
subject/client/current grant version plus the canonical agent ID and current
roles. It must be read-only, fail closed on missing/ambiguous/revoked bindings,
and participate in the same authority-writer exclusion contract. It must never
invoke a mutation-based identity upsert while holding the release fence.

Final admission re-introspects exact issuer/audience/life-only scope, validates
bounded token lifetime, refreshes bindings, and binds expiry to synchronous HTTP
handoff verification and response cancellation. Production composition must use
this authority and its response hooks. Earlier synthetic authorities' optional
hooks do not establish equivalent production admission.

PR20 still rejects `life:recall`. Its production scope/grant-version contract,
approved subject-to-agent/role provider, catalog/policy identity and complete
writer coverage remain explicit admission gates. Actual host topology, UID
isolation, immutable dependency manifests, paths/modes, credential-file references
and recovery ownership require host evidence and exact action-time approval.
The fixture does not resolve those inputs or authorize any personal namespace.

The issuer repository is private. Ordinary philotic-stack CI can run the new
binding tests through its existing service test job; composed fixture CI requires
an approved read-only checkout mechanism for the exact issuer source. This source
change creates no cross-repository credentials or grants.
