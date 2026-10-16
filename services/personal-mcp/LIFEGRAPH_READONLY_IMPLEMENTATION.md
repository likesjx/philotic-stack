# Approved read-only LifeGraph composition

This source slice adds `approved-read-only-v1` alongside the existing synthetic
profile. It retains the exact catalog/content bindings, canonical privacy ACLs,
provenance checks, current identity checks and response-lifetime release fence.
Only explicit `approved_<lowercase_identifier>` catalogs are accepted. It adds
no capture, deduplication, applet, mutation, default graph or raw query route.

`createApprovedLifeGraphGateway({config, issuer, resolveBinding})` composes the
real identity authority, owned storage (catalog, canonical SQLite policy owner,
Memgraph owner transport and release coordinator), and HTTP MCP gateway.
The approved profile requires final-identity and response-lifetime interfaces;
an initial identity callback alone is insufficient.

`start(config, env, {resolveLifeGraphBinding})` routes an explicitly enabled
LifeGraph configuration into this composition. Use only `life.recall`, a
separate resource, explicit subject/client allowlists and per-client policies.
Memory upstreams, memory policies and vault options cannot be mixed into this
path. The existing memory-only deployment package remains unchanged; the new
composition is loaded only for LifeGraph. Its installation must include the
production module and all owned-storage dependencies.

The composition uses a pinned issuer `binding` endpoint by default, or an
embedding application's canonical read-only `resolveBinding` provider. Given
`{subject, clientId, grantVersion, signal}`, the provider returns exactly
`{subject, clientId, grantVersion, agentId, roles}` using current authority,
or fail. The version is the issuer's `globalVersion:resourceGrantVersion`.
This callback runs inside the final release fence: it must not create identities,
upsert grants, take an issuer-change lease or infer roles from model arguments.
All changes to its backing identity/role source must participate in that fence.
No static JSON identity mapping or permissive fallback is supplied. The issuer
adapter uses confidential POST, same-origin and discovery pinning, and a bounded
uncached lookup. The CLI fails closed without the endpoint and approved bindings.

`config.lifeGraph` contains `enabled: true`, `profile`, `clientPolicies`, optional
`requestDeadlineMs`, and `storage` with explicit `policyDatabase`, `storeIdentity`
(`device`, `inode` strings), `catalogFile`, `catalogDigest`, `ownerSocket` and
`ownerIdentity`. Top-level resource/allowlists are authoritative. Issuer
introspection continues to use the existing credential broker.

Validation on mbp-jane: 129 of 133 gateway tests passed. The four failures in
`lifegraph-owner.test.mjs` are the canonical Linux policy-owner tests, which
reject this Darwin host before `/proc/self/fd` access. The exact-package import
test, new approved catalog/privacy tests, and existing memory regressions pass.
The Linux owned-storage fixture now also exercises an approved catalog.
The issuer checkout's 46 tests include real life-token issuance through HTTP,
memory-token rejection before graph reads, and final-admission revocation.
That HTTP test uses synthetic graph/release doubles; it is not full Linux owner,
MongoDB and Memgraph acceptance.

Release gates: run the complete suite on Linux and actual
Mongo persistence tests, then run the supervised composed acceptance with the
new issuer source and real life-token issuance. The composed fixture now issues
actual separate resource tokens, uses approved catalogs, and requires an exact
`--issuer-head`; its updated Linux run remains unexecuted. Supply the canonical binding implementation,
approved catalog/policy-store identity, and complete writer-enrollment evidence.
No live services, grants, credentials or personal graph records were accessed.

## Explicit delegated binding

Initial source inspection found no production backing implementation of
`resolveLifeGraphBinding`. Issuer `personal_oauth_identities` maps an authenticated
provider identity to an internal subject; global/client/resource admissions carry
versions and enablement, not a canonical agent ID or privacy roles.
`crates/ansible-mesh-core/src/storage.rs` defines separate projected user identity
and agent identity records. The user projection is a continuity mirror, not an
OAuth client grant. `privacy_local.rs` resolves authenticated launched guests;
`privacy_rpc.rs` requires a guest and local task envelope. Neither is an existing
OAuth subject/client resolver. `mcp_route.rs` constructs `mcp:<token_id>` for
legacy bearer callers, which must not be substituted for an OAuth principal.

The subsequent explicit-binding instruction is implemented by reusing the
issuer's existing versioned resource-grant record. Its optional `binding` holds
an operator-approved canonical agent ID and explicitly delegated privacy roles
for that exact subject/client/resource. It is not a mirror of the hotel's user
registry or all of an agent's roles. No new identity collection is created.
The confidential issuer binding endpoint reads and revalidates global/resource
versions and enablement at request and fenced release boundaries. Binding edits
and removal use the existing issuer-change helper and increment the grant
version, invalidating outstanding tokens. Website admin is an admission check,
never an implicit privacy-role grant. Token/client-supplied agent/roles are ignored.

Production bindings remain unset. Their canonical IDs, delegated roles and
writer coverage require exact access approval. The synthetic composed fixture
now uses this resource-grant binding directly; its old separate binding collection
has been removed. The remaining missing production inputs are approved bindings
and deployment evidence, not an unimplemented resolver. If canonical role
membership changes elsewhere should revoke these delegated roles, the responsible
owner must update/disable the resource grant through the same fence; there is no
automatic role inheritance or synchronization added here.

The updated composed worker bundles successfully using the actual issuer
dependencies and repository esbuild workflow. This verifies imports/transforms,
not Linux runtime acceptance. Issuer full typecheck passes; its TypeScript suite
passes 98 tests with four database tests skipped. The four gateway canonical
owner failures still require passing results on Linux before release.
