# LifeGraph gateway and storage wiring readiness

Source-only follow-up stacked on PR #650, on `codex/lifegraph-read-admission`.
This branch is independently reviewable and must not be folded into PR #650
without review. Parent authorized shared gateway/storage ownership; Muninn owner
retains issuer, broker, `server.mjs`, deployment/infrastructure, `rollout/**`,
`.github/workflows/muninn-bounded-package.yml` and Muninn transaction/test files.

## Implemented

`createLifeGraphGateway` defaults to disabled and returns null before constructing
storage or invoking an issuer. Enabled source composition returns an unbound
HTTP server; it does not start a service, open a network listener, create a grant,
resolve a credential or change configuration. The separate resource exposes only
`life.recall` with the fixed adapter descriptor and `life:recall` security scheme.
Existing memory-only policy paths remain intact. No combined memory/LifeGraph
resource or inherited memory credential is supported by this new profile.

`gateway.mjs` accepts the explicit `lifeGraph.enabled === true` option only for
the single LifeGraph tool with separate client policies and injected authorities.
OAuth introspection still validates issuer, subject, client, exact audience,
bounded expiry and active grant on every request. The LifeGraph profile additionally
requires exactly `life:recall` and a nonempty string `grant_version`. The identity
authority resolves current stable agent ID and roles from authenticated client,
subject and grant version; tool arguments cannot assert those privileges. Adapter
reauthorization culminates in the coordinated release barrier, after which the
gateway performs no further awaited operation before response serialization.
The issuer owner must confirm/provide this introspection field; missing denies.

`createLifeGraphStorageAuthority` opens **an existing canonical privacy SQLite
database read-only**, with `query_only` enabled, and uses the current
`privacy_revision(singleton,revision)` and `privacy_policy(resource,policy_json)`
schema already implemented by `ansible-mesh-core::privacy_storage::PolicyStore`.
It never initializes, migrates or changes that database. Missing database/schema,
policy, source ancestry or revision denies. Prepared parameterized policy lookup
prevents caller IDs becoming SQL. Complete source closure is bounded to 4096
policies and policy JSON to 64 KiB per record; node/edge counts retain adapter bounds.
Graph-supplied ACL fields are rejected instead of trusted. Read snapshots combine
graph revision and current canonical policy revision into a bounded hash. Policy
sources outside projected graph records appear only as `sourcePolicies` records,
without any content, and cannot become recall hits or edges.

`graphReader.readSnapshot(context)` is a server-owned read-only interface. It must
return a transactionally consistent, namespace-scoped export with exactly
`{namespace, revision, nodes, edges}`. Nodes carry only approved schema-bound
`{id,namespace,kind,summary,canonicalId?}`; edges carry
`{id,namespace,from,to,relation}`. It cannot provide privacy policy. Stable record
IDs must be bound by the storage owner to the canonical policy resource keys;
neither labels, soft zones nor ordinary provenance text establish that binding.
Current policy source manifests must cover the complete summary/content lineage.
The source profile still admits synthetic namespaces only; production is denied.

`releaseBarrier.admit(admission, validateCurrentPolicy)` must implement coordinated
graph + identity/grant + policy admission. Inside its final admission critical
section it invokes the synchronous canonical-policy validator with current
`{graphRevision, actor}`; this rechecks current SQLite revision and the exact actor
binding. Returning true without invoking that validator cannot admit. The barrier
must serialize relevant graph changes, policy revocation and issuer grant/role
changes against its release linearization point. It must not invoke the validator
early and then await another authority without revalidation. A read-only SQLite
transaction alone cannot serialize an external issuer and Memgraph. Missing or
unknown barrier capability denies; this task does not invent a distributed lock.
Production `PolicyStore` writers already couple policy change and revision in one
transaction; all other writers must preserve that invariant.

## Evidence and material blockers

Synthetic tests exercise real loopback HTTP MCP discovery/calls and a real temporary
SQLite policy store. Both dot and Claude receive only their namespace records;
memory-only/combined tokens, wrong audiences, missing grant versions, raw identity
hints and write tools are rejected. Tests cover default-disabled composition,
foreign private-source inheritance, graph ACL forgery, missing policies, separate
SQLite-connection policy changes, independent client revocation and withholding
output during final admission. These are source/integration tests, not an installed
runtime or Memgraph acceptance proof.

Local verification on Node 24.19.0: `npm test` passed 104 tests (including 10
new HTTP/storage integration tests); `npm run check`, explicit syntax checks for
the new modules and `git diff --check` passed. No Rust source or issuer/broker/
infrastructure files changed.

No Memgraph driver or executable is installed in this workspace. Docker works but
has **no installed images**; Python has neither `neo4j` nor `gqlalchemy`. A disposable
Memgraph test therefore needs a specifically approved software pull/driver or an
owner-provided synthetic service. No image, package or driver was installed, no
ports were exposed and no production graph was queried. Live vps-jane connection
details were neither invented nor read.

Required owner inputs before a production adapter can be implemented truthfully:

1. Exact existing Memgraph record/edge schema and authoritative ID-to-policy-resource
   binding, canonical links and complete immutable provenance manifests; select the
   approved six-kind ontology mapping and a namespace query fence. Ordinary schema
   docs currently describe provenance and zoning but do not establish privacy ACL
   installation or this binding. Unlabelled legacy records remain denied.
2. The actual canonical privacy store's deployment/availability contract. The source
   PolicyStore is a dedicated SQLite database, not a production LifeGraph migration;
   there is no verified existing vps-jane policy database or approved synchronization
   owner. A copied policy DB or arbitrary exported snapshot is not current authority.
3. A concrete, demonstrated storage/session/issuer release barrier and its revoke
   linearization contract. An injected function that returns true is fixture evidence
   only. Independent asynchronous reads cannot satisfy this admission contract.
4. Issuer confirmation of current client grant version and server role mapping,
   plus actual dot/Claude OAuth mode and exact callback URLs for later provisioning.
   Source tests do not require callbacks, and examples cannot substitute for them.
5. Approved synthetic Memgraph validation capability (preinstalled image/driver or
   owner-provided synthetic service), with no public port exposure.

## Admission and rollback

Independent source review and exact-head CI precede any merge. Keep this source
composition disabled while the owner inputs above are absent. After a reviewed
storage/barrier implementation passes synthetic acceptance, obtain action-time
approval for the exact nonsecret resource/profile, issuer registrations and grants,
policy/role binding, scoped read-only graph credential, broker role and supervised
server/security changes. The Muninn owner performs infrastructure work separately.
Roll back by disabling only the LifeGraph profile and revoking its independent
grants; memory-only Muninn client policies remain separately scoped. Mutation,
semantic deduplication, migrations, backfill and writes require their own bounded
requirements and review. No source publication permission is pending.

The parent retains shared architecture/task updates to avoid overlap. No graph/
memory bootstrap or writes were performed because the task explicitly prohibits
real personal data reads; source truth was used throughout. No merge or rollout.
