---
title: Native Memory Qualification and Activation Gate
doc_type: reference
domain: memory-context
status: active
last_updated: 2026-10-29
tags: [native-memory, qualification, security]
related_docs: [NATIVE_MEMORY_CAPTURE_PROPOSAL.md, NATIVE_MEMORY_CAPTURE_REVIEW.md]
task_refs: [../task.md]
---

# Qualification scope

This continues PR #683 from verified head `20524ebd`. It adds result authentication, real hotel composition and disposable Memgraph crash recovery. R15 semantic enrichment and R16 Obsidian originals remain future work. No NLP, graphRAG or SQL learning is added. SQLite remains a small insert/select fixture plus request-safety receipts/nonces.

## Result integrity

Real composition exposed hotel-added delivery metadata conflicting with strict signed-envelope parsing. Requests now travel inside a `native_memory_request` wrapper: delivery metadata stays outside, while the inner signed `Envelope` still rejects unknown fields and exclusively determines authority. The independent reviewer checked this boundary after the fix.

The destination uses a separate local private response-signing file (`--response-signing-config`, fields `key_id` and `signing_seed`). Ingress local config pins `response_verifiers` entries by destination, runner, key ID and public key. No default responder key or trust grant is generated. These new real credential/pin effects require approval before activation; tests use ephemeral synthetic fixtures only.

The domain-separated Ed25519 result signature binds the entire typed request envelope digest, result, key ID, issue time and original expiry. That digest includes caller, owner, namespace, destination/runner, event, nonce, operation, content/provenance and reply route. Ingress reloads pins for each reply, validates receipt/owner/size semantics, then consumes the pending call once. Removed/ambiguous pins, wrong keys, cross-request results, expired replies and result substitutions fail closed. Generic datasource replies and text/error/streaming completions cannot complete a native pending call.

This provides integrity, not confidentiality. Local UDS smoke routing does not qualify remote mesh transport. Raw optional HMAC/TCP remains ineligible for private capture. Protected mutually authenticated ingress/hotel/backend transport and its exact deployment topology remain a release gate.

Ingress also revalidates the saved request against current local grants and issuer policy before disclosing a signed result. Revocation while the request is pending leaves it unfulfilled even if the destination already committed; the immutable event remains available for authorized reconciliation. The membrane test exercises this boundary separately from response-key removal.

Private config loading uses `O_NOFOLLOW`, file-descriptor metadata/permissions and a bounded read on Unix. An independent second reviewer found the prior pathname check/read race and generic completion bypass risk; these are addressed. The reviewer also caught an expired-envelope clock in the restart test, corrected before CI. This is a code review, not formal PR approval or a claim of security certification.

## Reproducible qualification

Local final checks: 656 passed, 0 failed, 6 ignored (two existing helpers, two durable graph phases, one graph adapter test, one process composition). The process composition was then invoked explicitly and passed separately. Native strict Clippy, affected-package Clippy and formatting passed; seven pre-existing membrane warnings remain. Real Memgraph composition and SIGKILL recovery require the existing CI environment and are reported there by exact head.

```sh
CI=true cargo test --offline -p native-memory -p membrane-mcp -p membrane -p ansible-mesh-core --no-fail-fast
cargo clippy --offline -p native-memory --all-targets -- -D warnings
cargo fmt --check -p native-memory -p membrane-mcp -p membrane -p ansible-mesh-core
cargo build --offline -p aiua -p native-memory
PHILOTIC_TEST_BIN_DIR="$PWD/target/debug" cargo test --offline -p membrane-mcp real_hotels_native_runner_authenticated_mcp_composition -- --ignored --nocapture
```

The composition uses two actual aiua smoke processes, the production membrane runtime and HTTP MCP handler, and the actual native runner. Each hotel has a fresh temporary cwd/database/log/socket, cleared environment, no profile and no materialized agent/provider guests. It checks initialize/tool discovery, bearer-authenticated capture/recall, signed result delivery, same-event retry after runner restart, and failure when the destination stops. UDS peer forwarding is an existing smoke seam, not a claim about encrypted remote mesh execution.

The existing Memgraph CI job also selects this composition against its empty fixed localhost graph, using only native fixture labels/constraints. It then runs the existing LifeGraph acceptance. No arbitrary connection URL or personal graph is permitted by the fixture.

`scripts/native-memory-durable-smoke.sh` is CI-only and reuses the already present pinned image without pulling another image. A uniquely named disposable volume/container holds enabled WAL and an explicit schema snapshot. Capture occurs after the snapshot. Separate test processes seed and verify state around SIGKILL/restart of the same Memgraph volume. The test requires nonempty snapshot/WAL artifacts and checks recovered observation/receipt/nonce, original receipt time, retry deduplication, replay/conflict rejection and recall. This is process-crash recovery; physical power loss, disk faults and a lost Bolt COMMIT acknowledgement are not fault-injected. COMMIT errors conservatively return `Uncertain`; stable event reconciliation remains mandatory.

Exact head, commands and terminal CI evidence are recorded in the PR and executor closeout. Never infer a passed ignored test from ordinary cargo test output.

## Supported client gate

The tested surface is Streamable HTTP MCP `/mcp` with `initialize`, `tools/list` and `tools/call`, using only `native-memory.remember` and `native-memory.recall`. The supported OpenAI MCP configuration route is documented in [OpenAI MCP documentation](https://learn.chatgpt.com/docs/extend/mcp?surface=cli). Parent plugin discovery found no installed memory connector. HTTP fixture success does not make these tools callable from Percival/dot; its actual registration, reachability, authentication and user-authorized harmless capture must still be verified. Claude follows through the same contract.

## Candidate packaging and activation — no production action requested

`release/packages.toml` includes native-memory as a required Linux binary; macOS shipping is not added by this slice. The exact-head Native Memory Capture workflow builds a scoped release-profile Linux candidate containing only native-memory and membrane-mcp after capture checks pass. It reuses release-manifest.py, verifies the unpacked archive, and uploads checksums, full source SHA, target, build time and workflow/run provenance. Candidate metadata uses v0.0.0-alpha.<run-number> solely for the existing manifest format: no Git tag, GitHub release, install or production publication is performed. GitHub Actions retains the artifact for 14 days. Checksums detect byte changes; they are not independent signed provenance or a deployment authorization.

Retrieve the exact run's artifact named `native-memory-linux-x86_64-<full-head-sha>` with `gh run download <run-id> --repo likesjx/philotic-stack --name <artifact-name> --dir <empty-directory>`. Verify outer SHA256SUMS, unpack the tarball into a separate empty directory, then run release-manifest.py verify and compare manifest.sha with the reviewed head. Run/artifact IDs, expiry and hashes are reported in executor closeout after upload, not guessed before CI completes.

The accepted first topology is VPS-local ingress and runner, with central Memgraph on that VPS. This removes a laptop dependency and remote hotel hop. Source supports same-node EmitTask routing and explicit component executable/arguments. ProvisionMcpEndpoint currently writes empty args; activation must preserve the native config argument via RegisterComponent. Re-provisioning can overwrite it. Only new guests should be started; installed hotel compatibility remains unverified.

Read-only SSH metadata verification to jane-vps was blocked before connection by the execution environment (`Operation not permitted`, port 22). No retry, alternate route, secret reads or remote mutation followed. Live node/owner IDs, installed versions, port ownership, database/privileges/durability and local Bolt transport remain unknown. Port 8913 is withdrawn, not presumed free. No security mutation approval is requested until those facts resolve. Source locations are philotic-hotel.service, /run/philotic/vps-jane.sock, philotic-memgraph.service and /opt/philotic/data/memgraph; these are not live verification.

Official documentation supports custom MCP plugins on named ChatGPT/Codex surfaces, but does not establish dot-specific availability or this account's bearer compatibility. Conversation plugin selection is not proof of callable dot tools. No account or authentication changes were made.

Artifacts are the exact reviewed build of `membrane-mcp` at one selected ingress and `native-memory` at one selected destination. The scope is two preapproved authenticated MCP tools and one verified principal's source-observation namespace. No general agent grants or canonical fact/goal mutation.

Before an executable approval request, resolve the actual ingress/destination service names, binary paths/digests, IPC sockets, node/guest IDs, MCP HTTPS URL, protected hotel/Bolt channels and selected Memgraph database. Those live values have not been inspected under this task's restrictions; fixture values are not substitutes.

Enumerate the specific resulting changes: request-signing key custody at ingress; response-signing key custody at destination; mutually pinned public verification keys; narrow local operation grants and route preapprovals; three Memgraph unique key constraints and source/receipt/replay storage; actual WAL/snapshot/recovery configuration; starting only the new runner and reloading only the selected membrane; supported client connection setup. Any network/security setting change is a separate explicit effect, not implied by signatures or publication.

Rollback disables the two native routes and new grants/pins, stops only the new runner, and restores the selected membrane artifact/config. Retain Memgraph observations/receipts for reconciliation. No personal-data deletion, schema removal, unrelated agent restart, production install, merge or deployment is performed here.
