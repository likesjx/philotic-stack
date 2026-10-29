---
title: Native Memory Capture Review and Qualification Plan
doc_type: reference
domain: memory-context
status: active
last_updated: 2026-10-29
tags: [native-memory, security, qualification]
related_docs: [NATIVE_MEMORY_CAPTURE_PROPOSAL.md]
task_refs: [../task.md]
---

# Review disposition

Follow-up: [NATIVE_MEMORY_QUALIFICATION.md](NATIVE_MEMORY_QUALIFICATION.md) records the independent second-reviewer pass, signed-result fix, descriptor-based config loading, real hotel composition and crash-recovery qualification. The initial unsigned-response and pathname-race findings below are superseded by those fixes. Protected remote transport remains a release gate.

A separate adversarial self-review of PR #683 was performed after implementation. This is not a second-person review or formal PR approval. Publication remains a draft; activation is blocked on response integrity and protected transport. The initial exact head `1e48d646bad611311b46530899bb0ea6ee2782c8` passed repository macOS workspace tests, Linux compile, workspace Clippy ratchet, formatting and worktree safety, plus the dedicated native capture workflow.

## Findings and evidence

| Area | Finding / disposition |
|---|---|
| Request canonicalization | Versioned, domain-separated typed serde JSON binds every request and authority field. Harmless JSON field ordering/whitespace normalizes; duplicate/unknown fields reject. It is not a general cross-language JCS format. Publish byte-level fixtures before another implementation signs requests. |
| Authority | Caller arguments cannot supply owner, grant, issuer, destination or reply route. Tests mutate every signed claim and key content/provenance field, pin issuer to grant, and reject provider disclosure, wrong owner, revocation, expiry and changed local policy. Cross-owner agent delegation remains disabled. |
| Cancellation | Fixed a missing cancellation checkpoint after final policy reload. New regression proves cancellation in that callback rolls back. Cancellation concurrent with commit and remote cancellation delivery still require outcome reconciliation. |
| Key file | Fixed unrestricted ingress config reads: regular file, maximum 1 MiB, no symlink, Unix owner-only permissions. Tests reject public permissions and symlinks. Parent directories and same-user path replacement remain trusted-local assumptions; metadata/read is not a race-proof open. |
| Replay/durability | SQLite transaction persists nonce digest, event receipt and observation together; restart/retry/conflict tests pass. Binary smoke confirms receipt visible through a second connection before acknowledgement. No power-loss qualification or retention bounds yet. |
| Secret canaries | Tests cover real synthetic bearer omission from routed envelopes and signing-seed omission from operation storage. This is not an audit of every aiua queue/log/persistence surface. Signed envelopes and source content are sensitive. No signature provides confidentiality. |
| Response integrity — activation blocker | Generic `datasource_response` resolves pending requests by `turn_id`; membrane push handling discards source node. Request signing does not authenticate the returned receipt/result. An observer of an unprotected request could learn its nonce and forge a matching generic reply. Require authenticated response binding and protected channels before real data. A source-node string alone is insufficient. |
| Storage semantics | Stored items are exact source observations with owner, subject, authenticated writer, source/version/lineage and stable ID. Provenance is caller-attributed, not independently verified. SQLite is interim capture storage, not LifeGraph/Memgraph canonical facts, graphRAG or source archive ingestion. |

The review fixes add five tests: final-reload cancellation, typed JSON ambiguity rejection, all signed-field tampering, issuer/grant separation, and private signing-file handling. Local affected-package suite: **650 passed, 0 failed, 2 existing ignored helpers**; native crate: **15 tests**. These tests do not prove database graph enforcement or cognitive parity.

## Next bounded synthetic composition

1. Inspect aiua bootstrap and existing disposable test harness to prove all graph/config/log/socket paths stay under a fresh temporary directory. Use explicit supported socket and database/log environment overrides; do not use home-profile defaults or modify HOME. Confirm no agent/provider autostart before launching any fixture.
2. Run two disposable aiua hotels, ingress membrane and destination native runner with synthetic ephemeral keys/grants, loopback sockets, no provider configuration and no external data. Stop all fixture processes on completion. The current binary test uses a hotel stub; this next test must traverse actual hotel routing and its persistence surfaces.
3. Bind responses cryptographically to request digest, event, nonce, expected destination/runner and reply endpoint, with expiry and pinned verification policy. Reject modified, wrong-peer, stale and cross-request results before resolving pending calls. Use existing reviewed cryptographic primitives; no custom encryption construction.
4. Qualify authenticated encrypted transport in both directions, including endpoint identity/pinning and no downgrade to raw TCP. Existing optional HMAC/TCP is not encryption. Capture synthetic wire bytes and assert source/credential canaries are absent from plaintext. A successful signature test alone cannot satisfy this gate.
5. Exercise remember → recall, destination restart, duplicate delivery, response loss after commit, late reply, policy revocation while queued, malformed requests, unavailable destination and cancellation. Assert one observation/receipt for retries and no acknowledgement before durable commit. Audit temporary aiua queues/logs for reusable secret canaries, and document any necessary sensitive envelope retention.
6. Only after those tests pass, qualify MCP initialization, tool discovery and calls from the actual Percival/dot surface. Claude uses the same tools afterward. No paid model calls are needed for protocol-level qualification.

Reproducible current baseline (from repository root):

```sh
CI=true cargo test --offline -p native-memory -p membrane-mcp -p membrane -p ansible-mesh-core --no-fail-fast
cargo fmt --check -p native-memory -p membrane-mcp -p membrane -p ansible-mesh-core
cargo clippy --offline -p native-memory --all-targets -- -D warnings
cargo clippy --offline -p native-memory -p membrane-mcp -p membrane -p ansible-mesh-core --all-targets
```

## Callable client path

The repository exposes MCP `/mcp`; it does not install a client connector. Official [OpenAI MCP documentation](https://learn.chatgpt.com/docs/extend/mcp?surface=cli) describes Streamable HTTP servers with bearer/OAuth authentication and client MCP-server configuration. It also warns that local executor MCP availability does not establish cloud connectivity. This provides a supported interface candidate, not proof that the parent dot surface has access. The parent found no eligible installed memory plugin. Verify its supported server registration, endpoint reachability, authentication, tool discovery and actual harmless capture/recall before claiming Percival acceptance. No client settings or credentials were changed by this work.

## Activation effects package — pending, not an execution authorization

Proposed scope is one ingress membrane and one destination native guest. Build artifacts are `target/debug/membrane-mcp` and `target/debug/native-memory` for local qualification; a production build must be selected by exact commit, target and artifact digest. Intended roles are VPS ingress and mbp-jane destination. Live service names, installed paths, hotel/guest IDs, local sockets and protected endpoint URL have not been inspected and must be resolved before an executable approval request.

The approval request must enumerate these exact resolved effects:

- Replace/reload only the selected ingress `membrane-mcp`; add its local private issuer file and `--native-memory-config` argument. Create a narrowly scoped real signing credential only after approval; private mode 0600, local custody, no routed secret.
- Register/start only the destination native guest with its explicit socket, node, guest, owner-only SQLite path and public verification policy. Pin the ingress public key and one verified principal/namespace to remember/recall. This creates persistent grants and private observation/receipt storage.
- Add two explicitly authenticated and preapproved endpoint routes, `native-memory.remember` and `native-memory.recall`, pinned to that destination/guest. Any HTTPS/mesh transport, firewall or security configuration changes must be listed separately; none are implicit.
- Register the supported client MCP connection with only these tools and approve one user-selected harmless observation for first capture. No broad agent grants, production migration, canonical goal edits or paid provider use.

Do not activate until response/transport qualification is complete and the resolved effects are specifically approved. Rollback disables these two routes and issuer grants, stops only the new guest, and restores the previous selected membrane artifact/config. Retain SQLite observations/receipts for reconciliation; do not delete private data as rollback. No unrelated agent restart, merge or deployment occurred.

## Updated backend decision

User reaffirmed Memgraph before live rollout. SQLite effects above are historical fixture discussion and are withdrawn from the activation proposal. The next approval package must resolve the exact Memgraph database/schema, local Bolt credential custody, three unique key constraints, durability settings and retention/rollback effects. Do not create them on an existing graph without specific approval. Native Memgraph observation, receipt and replay writes share one transaction; the disposable CI job disables WAL/snapshots, so atomicity tests do not prove crash durability. No personal graph is accessed.
