# Privacy foundation — first bounded slice

Base: `develop` at `f170cb885b1be5bce665a86dfd283eef115df817`.
Workstream: `codex/privacy-foundation`.

This note describes the original reviewed foundation. This integration worktree
adds dedicated policy/inbox storage and a provider decorator; see
[PRIVACY_INTEGRATION.md](PRIVACY_INTEGRATION.md) for current behavior and limitations.

This is a transitional, pure library foundation. It does not change live
provider dispatch, authenticate network connections, persist grants or migrate
graphs. Synthetic tests prove policy evaluation and its composition with the
existing health-aware model routing oracle.

## Policy contract

- Stable identity and RBAC roles enter through a server-authenticated context
  adapter. The principal cannot deserialize from a request. The adapter contract
  is not a substitute for real server authentication.
- Owner, creator read privilege, and read roles are evaluated separately.
  Only the owner can revoke creator privilege. An independent RBAC grant remains
  valid after that revocation.
- Read permission never grants external processing. Private data forbids every
  external operation, including inference, embeddings, semantic resolution,
  speech recognition and speech synthesis. Nonprivate data still requires an
  explicit operation-specific external permission.
- All source policies are intersected on copies and derivatives, including
  transitive ancestry. Current source grants are read on every decision so
  revocation affects existing derivatives. Missing/invalid policies, cycles,
  excessive lineage and unknown endpoints deny processing.
- `rank_models_with_privacy` filters before the existing routing reflex ranks.
  A denied cloud candidate cannot reappear when the local candidate is unhealthy.
  `authorize_processing` supplies a reusable final provider-boundary check;
  callers must repeat it for retries and fallback attempts.

## Integration obligations (not implemented here)

The authoritative server must bind the stable principal to its verified session;
load roles and policy at a consistent revision; construct a complete input-source
manifest; persist immutable derivative ancestry; and classify the actual endpoint
(a local proxy to cloud is external). An empty manifest denies processing.
No brand, localhost address, profile hint, or model-produced field proves locality.

The dispatch check and invocation need a defined revocation consistency contract.
The present in-memory fixture demonstrates rechecking after revocation; it does
not solve a distributed check/invoke race. Never treat a successful ranking as a
durable permission token.

## Work ownership

Inspection found dirty Apple companion files and a dirty desktop login route.
Provider runtime, IPC, and shared architecture/task documents also differ in
active worktrees (surfaces, ElevenLabs, architecture cleanup and Beacon repair).
This slice avoids those files. No overlap was found for the new privacy module,
its synthetic test, or `model_oracle.rs` during inspection. Shared architecture
and task updates are deferred pending coordination, rather than claiming global
privacy enforcement or modifying another workstream's documentation.

## Subsequent implementation plan

1. Coordinate ownership of server identity, storage and provider runtime. Wire
   authenticated principals and immutable source manifests into final checks on
   every provider path, including auxiliary calls, retries and fallback. Test
   fake provider invocation counts: denial must produce zero external calls.
2. Add durable privacy policies, owner-authorized revocation and revision-aware
   enforcement. Persist restrictions atomically with every copy/derivative.
3. Add resolve-before-create LifeGraph capture: equivalent meaning resolves an
   existing root; new details attach extensions; distinct meaning creates a root;
   ambiguous matches enter review. ACL-filter before local embeddings/dedupe;
   forbidden external processing cannot be a fallback.
4. Make capture durable and writes atomic/idempotent with evidence, stable
   operation IDs, uniqueness/conflict handling and retry-safe resolution.
5. Add Memory/LifeGraph privacy applets. Add selectable ElevenLabs-first voice
   for explicitly permitted content and Apple local voice for private content;
   unavailable local speech processing must fail closed or use an allowed local
   text path, never cloud fallback.

No pushes, merges, deployments, restarts, live grants or graph mutations belong
to this slice. Workspace-wide baseline/live verification is not claimed.
