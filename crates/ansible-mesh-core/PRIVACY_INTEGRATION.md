# Privacy backend integration preparation — 2026-10-08

Worktree: `privacy-integration`; base `78eb2f32` (cached origin/develop).
This builds on the independently reviewed privacy foundation. The separately
reviewed graph shadow planner remains in `graph-resolve-plan`, unexported.

## Implemented source behavior

`privacy_storage::PolicyStore` owns a dedicated SQLite privacy database and
durable capture inbox. It refuses initialization against an existing nonprivacy
database and does not migrate hotel or LifeGraph storage. Tests create only new
synthetic temporary databases.

- Policy insertion requires an authenticated owner; records cannot be replaced,
  and source ancestry cannot be edited. Inherited policies are loaded from the
  canonical source record on every new snapshot.
- Owner-only creator revocation and monotonically increasing policy revision
  commit together. Both survive database reopen. Snapshots expose an immutable
  revision, and invalid/missing policy data produces an error.
- Capture enqueue checks revision and all declared source read permissions in
  one real SQLite transaction, then durably stores the payload and manifest.
  Producer/event identity is unique; identical replay returns Replay; changed
  actor/payload/manifest conflicts. Two connections concurrently enqueuing the
  same event produce one durable inbox row.

The inbox is pending capture, not a second LifeGraph or a committed graph receipt.
It does not resolve roots, attach extensions, or guarantee canonical graph-key
uniqueness. The graph planner's transaction simulation is not production storage.

`model-router::privacy_dispatch` adds a final `ModelProvider` decorator and
`guarded_registry` factory that wraps all selectable and fallback providers.
The decorator obtains server-authoritative context for the complete typed task,
then applies shared policy immediately before ordinary or streaming invocation.
It rechecks every invocation, including retries/fallback. Missing context/source,
unknown endpoint, inherited private restriction and denied ACL fail closed.
Native-live tasks are explicitly denied here and need their own enforcement.

Tests exercise the actual ControllerTask parser, ModelProvider trait and existing
ProviderRegistry with mocked providers. They prove zero external invocations for
private tasks, timeout fallback, retries, streaming, derived summaries and caller
assertions; local calls and explicitly permitted nonprivate text calls reach mocks.
They bind summaries, tool definitions and tool-result history to the fixture's
server-owned task/manifest and observe durable revocation between attempts.

## Explicit production gaps / ownership

The runtime still uses its existing registry: the new factory is NOT installed.
The DispatchPrivacyAuthority production adapter is NOT implemented. Test fixtures
provide authenticated context; this code never turns JSON agent/private/egress
claims into an authenticated principal. Server session authentication, authoritative
roles, complete payload-source manifests and endpoint locality must be supplied
by the hotel owner before runtime installation. An authority that returns an old
snapshot violates the adapter contract; this decorator cannot authenticate an
untrusted implementation of that interface.

Revocation is proven for subsequent invocations. In-flight provider cancellation
and a distributed check/invoke revocation barrier are not implemented. The server
must define that consistency contract before production rollout.

Current IPC split branch `codex/ipc-split-s3` still owns hotel IPC dispatch modules,
including mod.rs, routing_policy.rs and config_vault.rs. Beacon owns its separate
SDK/memory-integration patch; it is excluded from the publication branch. The renewed source-only integration scope permits isolated SDK and mod.rs edits.
It avoids provider implementations,
runtime.rs, Apple files, and shared architecture/task documents. The renewed scope
authorizes isolated integration preparation, not overriding those workstreams.

Next coordinated boundary: hotel issues a verified-session task handle bound to
the full payload/source manifest/current policy revision; the controller resolves
that handle through authenticated IPC and installs guarded_registry for every
provider/fallback path. Unknown authority must deny, without a permissive legacy
fallback. The v2 opt-in resolve/cancel RPC now exists in source; live installation and
protected delivery/park/repark still require owner coordination. See
[LOCAL_AUTHORITY_INTEGRATION.md](LOCAL_AUTHORITY_INTEGRATION.md).

Next graph boundary: Memgraph owner chooses canonical verified-key uniqueness and
one atomic transaction for binding/root/extension/evidence/receipt writes, with
commit-time policy and graph-resolution revalidation. Durable inbox delivery needs
lease/ack/recovery semantics before connecting it to that transaction. Applet
privacy controls and duplicate review follow backend installation.

The unexported graph planner, commit orchestrator and Memgraph adapter are now included
in this worktree. Their precise validation limits and remaining adapter work are
documented in `../data-memorygraphrag/CAPTURE_COMMIT_INTEGRATION.md`. The durable
inbox handoff rechecks source permissions on load. The source-to-canonical-write
path is tested with a durable graph fixture and real SQLite policy authority;
actual Memgraph validation and runtime installation remain pending. A concrete
SQLite reservation now orders graph commit against owner revocation.

## Voice-facing contract

The authenticated server owns stable identity, roles, current policy revision,
complete source IDs and transitive derivative restrictions. Read permission never
grants egress. Classify the actual endpoint: unknown denies; a local proxy to a
cloud service is External. Private sources prohibit external STT/TTS, embeddings,
dedupe and inference. Every attempt must reauthorize; missing local capability
cannot fall back to an external provider. Swift routing preferences select among
permitted paths and cannot relax server policy. Voice settings remain separately
owned; this work changes no Swift files.

## Validation limits

Synthetic policy, inbox and actual provider-decorator tests run offline. No live
private records, credentials, model calls, graph writes, migrations, deployments,
restarts, pushes or global production enforcement are involved. Broad installed
runtime verification is not claimed. Clippy includes pre-existing philote warnings;
the changed core/model-router checks pass without errors.
