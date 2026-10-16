# Synthetic composed acceptance

This fixture depends on the reviewed PR658 supervisor source inherited by this
branch and an explicitly pinned reviewed issuer commit implementing the
read-only LifeGraph contract. It never installs
services or reads configured databases. The runner exports the supplied exact
clean source commit and builds its supervisor with the pinned Rust image,
locked dependencies, a fresh target directory and networking disabled. It does
not accept an externally compiled supervisor. Supply an existing public Cargo
registry cache and a clean exact issuer checkout with locked npm dependencies.

```sh
python3 services/personal-mcp/lifegraph-owner-tests/run-composed.py \
  --approved-fixture-run --source-head "$(git rev-parse HEAD)" \
  --cargo-registry /ABSOLUTE/PUBLIC/CARGO/registry \
  --temporary-parent /ABSOLUTE/DISPOSABLE/SCRATCH \
  --issuer-source /ABSOLUTE/EXACT/ISSUER/CHECKOUT \
  --issuer-head EXACT_REVIEWED_40_CHARACTER_ISSUER_COMMIT
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

Actual authorize/consent/token/introspection handlers issue distinct dot/Claude
LifeGraph and memory tokens for separate audiences and separately provisioned
disposable resource grants. Memory tokens fail LifeGraph dispatch. No token or
claim is relabeled: the gateway receives the exact opaque LifeGraph access token
and issuer-produced scope/version claims. The gateway uses approved catalogs
with synthetic content. This updated fixture requires a new Linux acceptance
run; previous fixture results do not validate the new issuer contract.

Assertions cover creator access, independent role access and revocation, private
records/ancestors/incident edges, external inference denial, canonical-root
projection, dot/Claude isolation, revocation before final release and both fence
race directions. A competing SQLite writer is excluded while the actual HTTP
response is held. Both contention directions wait for the owner's fixed diagnostic
emitted only after the authenticated request polls the held mutex and registers
as a waiter; scheduling delays cannot satisfy the exclusion assertion. The actual
Mongo helper's first write cannot run until handoff finishes. This delays buffered
`res.end`, not streaming writes, backpressure or remote receipt. Global admission,
provider logout and token revocation use actual
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

The approved subject-to-agent/role provider, catalog/policy identity and complete
writer coverage remain explicit admission gates. Actual host topology, UID
isolation, immutable dependency manifests, paths/modes, credential-file references
and recovery ownership require host evidence and exact action-time approval.
The fixture does not resolve those inputs or authorize any personal namespace.

The issuer repository is private. Ordinary philotic-stack CI can run the new
binding tests through its existing service test job; composed fixture CI requires
an approved read-only checkout mechanism for the exact issuer source. This source
change creates no cross-repository credentials or grants.
