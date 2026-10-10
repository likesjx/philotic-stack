# LifeGraph synthetic read admission contract

Source implementation only. Base: `develop` at
`3144e42930d575532249e0133dc15e10b45afb58` (merged Muninn gateway PR #648).
Ownership: this work owns `lifegraph.mjs`, `lifegraph.test.mjs`, and this contract.
The coordinating gateway owner owns `gateway.mjs`, `server.mjs`, issuer wiring,
configuration, `docs/task.md`, architecture disposition and rollout. No shared
gateway files, server configuration, credentials or persistent grants change here.

## Current behavior and limits

`createLifeGraphAdapter` admits only `synthetic-read-only-v1`. It implements the
privacy boundary with injected server authorities and synthetic snapshots. It is
**not connected to Memgraph, installed, advertised, or production-admitted**.
PR #648's memory-only dot/Claude policies continue rejecting `life:recall`.
The legacy gateway's LifeGraph text forwarding is not a verified privacy filter.
The Rust privacy foundation explicitly describes itself as transitional and not
wired into live dispatch; its policy semantics are preserved by this adapter.

Reads support Goal, Idea, OpenLoop, Person, Place and Thing records. Matching is
bounded case-insensitive substring matching against already authorized records,
not semantic search. Stored canonical links resolve variants to approved roots;
every link, lineage source and edge endpoint must pass current policy. This does
not determine semantic equivalence. No writes, embeddings, learning, deduplication,
canonical mutation, node retirement, or model fallback run in this module.

## Server authority interface

Construct with an exact HTTPS MCP resource, explicit per-client policies and:

```js
createLifeGraphAdapter({
  profile: 'synthetic-read-only-v1',
  resource: 'https://mcp.example.test/life/mcp',
  clientPolicies: [
    { clientId: 'dot', subjects: ['synthetic-operator'], namespace: 'synthetic_dot', scopes: ['life:recall'] },
    { clientId: 'claude', subjects: ['synthetic-operator'], namespace: 'synthetic_claude', scopes: ['life:recall'] },
  ],
  authenticate, snapshot,
});
```

The example is a fixture, not a production registration. Namespace sharing is
rejected in this profile. No memory scope or private/default namespace is accepted.

`authenticate(request, {signal})` is owned by the server. It must introspect the
actual token, enforce issuer, exact single resource audience, bounded lifetime,
explicit subject/client grant, revocation and grant version, then map the identity
to a stable agent ID and current roles using a trusted authority. It returns
`{active, audience, clientId, subject, scope, agentId, roles, grantVersion}`. Neither
agent IDs nor roles may come from tool arguments, model claims or unverified token
decoding. `request` is an opaque server request handle; it is never passed to the
graph snapshot authority. All external clients are classified as external inference
consumers; callers cannot select a trusted/local bypass.

`snapshot({namespace, clientId, subject, signal})` returns a current, consistent,
bounded synthetic export: `{namespace, revision, nodes, edges}`. `revision` must
change monotonically with every content, canonical, ACL or provenance change.
It must not export a whole production graph for post-filtering. Policies must be
canonical and current, including source references; missing source policies deny.
There is no snapshot cache. The adapter clones each returned export immediately.
Maximum export sizes are 2048 nodes and 4096 edges; larger exports fail closed.
Signal cancellation applies across all authority calls and the total request budget.
Authority implementations must honor cancellation to stop abandoned work.

Nodes: `{id, namespace, kind, summary, canonicalId?, policy}`.
Edges: `{id, namespace, from, to, relation, policy}`. IDs are unique across both.
Each policy follows the shared Rust field names:
`{owner, creator, creator_read_grant, read_roles, private, external_operations, sources}`.
All fields are required, except the node canonical link. Unknown record/policy
fields are rejected. Owner, revocable creator privilege, or current role grants
permit reading. Separately, private data always denies external processing, and
`external_operations` must explicitly include `inference`. Read grants do not
imply egress grants. Source restrictions survive copies and canonical resolution;
missing policies, cycles and excessive lineage deny. Soft zoning is not an ACL.

## Gateway handoff

Use a separately approved LifeGraph resource/profile; do not expand existing
memory-only policies or reinterpret memory credentials. The handler calls
`adapter.recall(serverRequest, arguments, {signal})`; only `query_text` and optional
`max_context_packets` (1–12) are accepted. `descriptor()` is a fixed local schema,
with no upstream diagnostics or descriptions. No descriptor is installed by this
change. The gateway owner must attach the exact `life:recall` OAuth security scheme,
protected-resource discovery and existing transport/body/concurrency limits.
Do not route this handler through legacy `frontdoorAdapter` text pass-through.

Return is MCP text containing JSON `{packets, edges}`. Packets contain only canonical
`id`, `kind`, `summary`; edges contain `from`, `to`, `relation` among returned,
authorized roots. There are no hidden-hop traversals, aggregate counts, degrees,
scores, aliases, private existence explanations, policies, authority metadata or
raw backend diagnostics. Filter before matching, ranking and limiting. Unauthorized
or missing resources contribute no hits. No-match output is always the same empty
shape. Pending output is withheld if either the graph/policy export or authenticated
grant/role mapping changes. Final introspection is the last awaited operation before
returning; ordinary distributed revocation still has the unavoidable race after
that final authority check. Errors are uniformly `LifeGraph unavailable`.

Content correctness and privacy labels depend on trusted canonical storage. This
is not a semantic content classifier or proof against timing side channels. The
projection returns approved summaries verbatim; a summary containing private facts
needs private/source policy at capture. Synthetic acceptance does not establish that
production LifeGraph currently stores adequate ACL/provenance or canonical links.

## Safe rollout proposal and approval gates

1. Independent review of this exact source commit, then exact-head hosted CI.
   Keep the PR draft and unmerged until both requirements are satisfied.
2. Gateway owner reviews and implements the explicit interface separately, with
   synthetic HTTP/MCP and issuer interoperability tests for both clients. Preserve
   PR #648 memory-only discovery and policies. Confirm actual client callbacks and
   OAuth/transport compatibility; example URLs are not admission evidence.
3. Before any persistent admission, request action-time approval for the exact
   issuer grants, agent/role mapping, client-specific namespaces, credentials,
   broker roles, resource URL, supervised service and server/security changes.
   No helper should create a grant or broaden a credential implicitly.
4. Under approved synthetic-only rollout, verify installed commit/process identity,
   discovery, both read flows, negative scopes/audiences/identity hints, independent
   revoke/re-enable with incremented grant version, and revocation during recall.
   Roll back by disabling the LifeGraph resource/profile and revoking only its
   grants; retain the independently authorized Muninn memory profile.
5. Production data admission requires a separate reviewed bounded storage adapter:
   authoritative current ACL and lineage export, read-only database role, namespace
   enforcement before search/embedding, transactional revision invalidation and
   approved treatment of unlabelled/legacy records (default deny). Production
   Memgraph is known to run on vps-jane; no endpoint, schema mapping, credential or
   connection detail was invented or verified here. Do not query personal records
   to prove readiness under this task. Use owner-reviewed schema/migrations and
   synthetic fixtures first. Private content stays denied to external models.
6. Writes and semantic dedup mutation activation remain separate: establish explicit
   bounded requirements for root selection, provenance-preserving variants,
   operator adjudication, conflict handling and rollback before requesting approval.

Closeout: no Muninn/real LifeGraph reads or writes, live services or grants. No
harness telemetry was started. Shared architecture/task updates are handed to the
coordinating owner to avoid overlapping edits. Next work is independent review and
explicit gateway admission, then storage-authority integration. Evidence is local
synthetic `test-green`, never installed-runtime or production acceptance.

Local evidence on Node 24.19.0: unchanged baseline `npm test` passed 80 tests;
final suite passed 92 tests, including 12 adapter acceptance tests. `npm run check`,
`node --check lifegraph.mjs` and `git diff --check` passed. No Rust source changed;
workspace Rust builds and live binary smokes were not run for this isolated module.
