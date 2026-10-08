# Local authenticated task authority — consumer contract v1

This isolated source checkpoint follows verified develop `689bfc7d` and the
separate reviewed Beacon memory-refresh commit. Runtime installation is off.
No existing GuestIdentity registration has been promoted to authentication.

## Implemented trust boundary

`privacy_local::LocalLaunchRegistry::authenticate(hotel, &UnixStream, guest)`
reads kernel peer PID and UID and matches both to a live child the supervisor
actually owns. Unknown PID support, exited/busy child state, UID/PID mismatch,
retired generation or guest spoof denies. Registry replacement invalidates old
proofs. LaunchPrincipal is server-loaded stable identity/roles and has no serde
constructor. The caller's role, agent ID and source-list assertions confer no
rights. Existing Tokio supports kernel peer PID on this Mac; no dependency or
launch credential was added.

`aiua::service::protected_local_guest::ProtectedLocalGuest` is an opt-in adapter
over an actual Tokio child. It retires launch authority before stop/reap/drop.
It is compiled but not installed in startup or the existing materializer. Its
server principal mapping and expected UID must come from authoritative records.
PR634's scoped vault startup/UID policy and release/Ansible files are preserved.
That fixed-reference credential broker is not an agent identity issuer.

## Minimum API for context and voice consumers

Exported core module: `ansible_mesh_core::privacy_local`.

* `LocalTaskEnvelope { task_id: Uuid, payload: String, authority_handle: Uuid }`
  carries the exact complete payload plus an opaque handle outside payload JSON.
* `LocalTaskAuthority::issue(verified_origin, server_consumer_target, task_id,
  payload, ttl)` obtains the complete immutable resource manifest from the
  LocalManifestAuthority server catalog and records origin/consumer launch
  generation, exact digest, policy revision and expiry. Exact replay returns the
  same handle; changed binding or a tombstone denies. Capacity rejects, never
  evicts cancellation tombstones. Protected cross-hotel issuance denies.
* `resolve(&envelope, &verified_consumer, operation, actual_boundary)` returns
  `ResolvedLocalAuthority { actor: AuthenticatedAgent, policies: PolicySnapshot,
  sources: Vec<String>, payload_digest: String, consumer_incarnation: Uuid }`.
  It checks both live launches, registry identity, incarnation, task, full digest,
  expiry, cancellation and CURRENT source policy. Read does not imply egress;
  External/Unknown boundaries remain subject to the shared fail-closed gate.
* `park(&envelope)` / `flush(&parked, &verified_consumer)` preserve the same
  envelope and handle; flush revalidates. Neither API can issue from deserialized
  JSON. A replacement incarnation cannot inherit the parked binding.
* `cancel(&verified_origin, handle)` creates a persistent-in-registry tombstone;
  wrong actors deny, repeated cancellation is idempotent. Each retry/publication
  must resolve/check again; this does not itself abort active provider generation.
* `pin_capture(&envelope, &verified_consumer)` returns the resolved context and
  concrete PolicyCommitLease. `validate_pinned_capture` checks the same authority
  and reservation without reacquiring the policy-store writer connection.

Boundary classification is a server configuration decision, not a wire flag.
Source manifests must include tool results, summaries, context, attachments and
microphone/synthesized text sources. A missing authoritative catalog denies.
The methods perform blocking SQLite work and must execute on blocking workers.

## Canonical graph bridge

`data-memorygraphrag/src/capture_local_authority.rs` implements CommitAuthority
using this exact envelope and kernel-verified consumer. It loads the durable
capture from the reserved SQLite connection and checks payload, digest, source
manifest and recorded principal before the canonical transaction. It delegates
root lookup/read/write authority to the existing server-owned PlanningAuthority,
never caller candidates. The actual Memgraph adapter remains unexported and
requires its schema/backfill/isolation prerequisites.

Tests cover a real owned child → kernel-verified UDS session → scoped authority →
durable inbox → commit orchestrator → durable reference graph, followed by
cancelled replay denial. This is actual local-kernel/SQLite evidence and a graph
fixture; it does not validate Cypher or Memgraph concurrency. Temporary sockets
required the approved test-only sandbox escalation. No hotel service was used.

## Exact installation boundaries still unimplemented

The present hotel/SDK InboundTask and ParkedInboundTask have not been rewritten.
The existing legacy Register handler remains unchanged. Context/voice consumers
can implement against this v1 contract now, but there is no live IPC resolve RPC
or provider registry installation yet. Those require coordinated owner edits:

1. Supervisor supplies canonical agent/role mappings for persona and component
   guests; installs the protected child registry without trusting DB PID strings
   or incoming GuestIdentity. Retire proofs on child exit and connection teardown.
2. Hotel/SDK transport carries the envelope, authenticates consumer sessions,
   and resolves policy context on every protected attempt. Park/repark/flush must
   retain it. Missing authority must not enter legacy provider dispatch. The
   in-memory handle registry cannot survive hotel restart: unknown old handles
   deny and require new trusted admission; do not silently regrant on redelivery.
3. Model runtime decorates initial, credential-rebuild and decisions registries;
   streaming STT and native-live paths need separate enforcement. Context and
   voice owners coordinate consumer adapters. Native-live remains denied.
4. Edge device bearer authentication must bind device/request/turn cancellation
   through the hotel; agent authority cannot be inferred from the verified device
   alone. The voice admission ledger is not provider cancellation.
5. An available permitted disposable Memgraph fixture is still required for
   actual query/constraint/isolation validation. No live schema/data changes.

No new installs, live credentials/grants, startup permissions, migrations,
deployments, pushes or service restarts are part of this source checkpoint.
