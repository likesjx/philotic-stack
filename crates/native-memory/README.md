# Native memory capture — opt-in vertical slice

Client-neutral capture contract: Percival/dot is the first intended client, Claude follows through the same membrane contract. No callable dot connector has been discovered or installed. Shell test access is not client tool access.

`membrane-mcp` authenticates its existing per-route bearer credential locally, resolves a **separate local** native delegation grant, signs a normalized request, and sends it through `membrane::runtime::build_inbound_request` as an `EmitTask` pinned to the destination hotel and `native-memory-runner` guest. `native-memory` independently verifies Ed25519 signatures and local policy, then remembers/recalls private source observations. No Muninn authentication/backend hop or paid provider is involved.

## Reproduce

```sh
CI=true cargo test --offline -p native-memory -p membrane-mcp -p membrane -p ansible-mesh-core --no-fail-fast
cargo fmt --check -p native-memory -p membrane-mcp -p membrane -p ansible-mesh-core
cargo clippy --offline -p native-memory -p membrane-mcp -p membrane -p ansible-mesh-core --all-targets
```

The native tests use synthetic keys and temporary SQLite files. The binary smoke uses a temporary Unix socket, the real native executable and Philotic SDK framing, with a disposable hotel stub. The membrane test uses the real bearer checker, dispatch and router builder, with a simulated destination hotel invoking the real receiver. Neither is a deployed three-hotel test. Existing socket tests need permission to bind local test sockets. The first offline dependency resolution failed because the workspace lock required an uncached package; repository dependencies were resolved with Cargo, without installing a toolchain or calling a model/provider.

## Fixed tools

Local validation on 2026-10-29: **650 passed, 0 failed, 3 ignored (2 existing fixture helpers plus the opt-in real-Memgraph test)** across the four affected packages. The new crate has 15 acceptance tests including its binary smoke; the membrane package has 32 tests including the assembled native path. Formatting/diff checks pass. Native-memory Clippy passes with `-D warnings`; affected-package Clippy passes the repository ratchet, retaining seven existing membrane-mcp warnings. The first binary smoke failure was a test-harness nonblocking accepted socket on macOS; setting that fixture socket to blocking fixed it. No installed runtime was touched. Confidence: test-green plus a disposable binary smoke, not live-green.

Two endpoint-config routes use target `{"kind":"native_memory","runner_id":"memory-1","target_node":"memory-hotel"}` and matching actions `native-memory.remember` and `native-memory.recall`. They require explicit BearerToken authentication and operator-provisioned preapproval; loopback `None` authentication is rejected for native routes. Legacy dynamic route tables cannot invoke this target.

Arguments are strict (unknown fields rejected):

```json
{"event":"stable-client-event-id","command":{"operation":"remember","id":"stable-observation-id","subject":"practice","content":"A harmless source observation","source":"source-reference","source_version":"1","lineage":"source-lineage-id"}}
```

```json
{"event":"new-query-event-id","command":{"operation":"recall","query":"harmless","limit":10}}
```

Owner, caller, namespace, destination, runner and grants are never taken from these arguments. Stored records preserve owner, subject, authenticated writer, original content, source/version and lineage; `kind` is always `observation`. A source is caller-attributed provenance, not verified external evidence. No canonical fact confirmation, goal editing, role/commitment changes, NLP extraction, inferred links, graph embeddings, consolidation or learning is enabled. Recall is deterministic bounded substring matching, not graphRAG or semantic search. Records are private to owner/namespace; no shared-memory dedup is performed.

## Authority and durability

Real Ed25519 (`ed25519-dalek`, already used by the repo) signs a domain-separated typed JSON claim set. SHA-256 binds the exact typed request including operation/resource/event/provider. Unknown JSON fields are rejected. Claims bind pinned issuer/key, authenticated principal/delegation, policy revision, destination/runner, owner/namespace, signed reply node/guest, issued/expiry time (at most 60 seconds) and nonce. Receiver pins each delegation to its issuer and grants. Provider disclosure is denied entirely in this milestone. Current grants permit only a principal's own observation namespace; delegated agent contribution to another owner remains future explicit policy work.

The runner reloads public-key policy before execution and before commit; expiry/revocation/policy revision changes fail closed. Policy publication should be atomic; the final check and SQLite commit are not atomic with an external policy file replacement. The runner does not accept unverified error return routes. Rejections fail closed and currently reach the caller through the membrane's bounded timeout rather than detailed remote errors.

SQLite is a **synthetic contract-test fixture only**, not an eligible live backend. The Memgraph adapter described below is required before live acceptance. `BEGIN IMMEDIATE` plus FULL synchronous commit atomically stores the observation, operation receipt and nonce digest. A success is returned only after commit; the binary test verifies receipt visibility from a second connection before accepting the reply. Exact event retries return the same receipt without another write. A fresh signed nonce may retry the same immutable event; changed payload under that event conflicts. Reused nonce with different claims rejects, including after restart. All checks rerun before returning a retry. Recall retries return currently authorized current matching observations with the original audit receipt; they are not snapshot pagination. Receipt/replay tables have no pruning yet. SQL constraints serialize concurrent unique identities; crash/power-loss fault injection and load testing are not yet qualified.

Reusable external bearer, issuer private key, master/backend/provider credentials stay local. Only the short-lived delegated envelope is routed; it is still sensitive, as is memory content. Ed25519 is authentication, not transport encryption. The existing optional mesh HMAC/TCP is **not** claimed as protected transport. No native production route is enabled by this change. No universal secret detector is claimed for user-supplied memory content. Canary tests cover actual external bearer omission and absence of signing-seed fields/serialized capabilities in the operation store.

## Runtime entrypoints (not installed or invoked against live hotels)

Ingress: `membrane-mcp --native-memory-config <local-issuer-file>` in addition to existing IPC/listener args. This file contains a signing seed and local token-ID→grant map; keep it outside routed config. Loading rejects symlinks, files over 1 MiB and group/world Unix permissions. It is reread for every native call. No default issuer or grant exists.

Destination: `native-memory --socket <local-hotel-socket> --node <destination-node> --guest <runner-id> --memgraph-config <private-local-bolt-config> --policy <local-public-policy-file>`. Policy contains pinned public keys and matching grants only. No GetSecret, FetchMemoryConfig or backend/provider network call occurs. No service definition, real signing key, persistent grant, route provisioning, installation or restart is performed here.

Cancellation is implemented at library checkpoints before execution and commit; the current guest does not wire remote cancellation notifications. A timed-out caller must retry its stable event ID to reconcile a possibly committed outcome. End-to-end cancellation, restart fault injection, resource retention limits, transport confidentiality, supervisor configuration and real hotel/connector qualification remain gates.

See [proposal and rollout effects](../../docs/architecture/NATIVE_MEMORY_CAPTURE_PROPOSAL.md). The separate 35-test cognition experiment stays unchanged outside this branch; this capture slice adds no source-derived algorithms.

See the [adversarial review and next qualification plan](../../docs/architecture/NATIVE_MEMORY_CAPTURE_REVIEW.md), including the unresolved response-integrity activation blocker.

## Memgraph capture adapter

`MemgraphRunner` reuses the existing vendored neo4rs driver and same-connection metadata verification pattern from LifeGraph. It verifies transactional storage, snapshot isolation and unique `key` constraints on `NativeMemoryObservation`, `NativeMemoryReceipt` and `NativeMemoryNonce`; it never creates schema automatically. Observation bodies preserve all source fields, with explicit owner/namespace/id/content properties for scoped recall. These are source observations, not confirmed LifeGraph canonical facts or a graphRAG implementation.

One Memgraph transaction contains the observation, immutable event receipt and nonce digest. A failed COMMIT acknowledgement returns `Uncertain`; callers must reconcile using the same event. Transaction conflicts fail closed and may require retry. No SQLite receipt accompanies Memgraph writes. Atomic visibility is distinct from crash durability: the existing disposable CI container disables WAL/snapshots, so its acceptance cannot prove power-loss recovery. Durable Memgraph configuration/restart tests remain required before activation.

The binary requires `--memgraph-config` for the intended backend; local private JSON fields are `uri`, `username`, `password`. These credentials stay local. The existing smoke explicitly selects `--sqlite-fixture <path>`; there is no implicit SQLite fallback. No real backend configuration/credentials were read or created.

The new opt-in `tests/memgraph.rs` runs only against an empty disposable localhost:17687 graph, checks schema rejection, signed capture/recall, fresh-connection retry, conflict/replay, owner isolation, policy-change rollback and concurrent duplicate delivery. It runs before the existing LifeGraph fixture in its CI workflow. Local Docker is available but no Memgraph image is installed; no image/software was installed. The adapter compiles locally; real graph acceptance is reported separately with the exact CI head.

```sh
PHILOTIC_SYNTHETIC_NATIVE_MEMGRAPH=1 cargo test --locked -p native-memory --test memgraph -- --ignored --test-threads=1
```

Run that command only in the documented disposable empty CI environment, never against an existing personal graph.
